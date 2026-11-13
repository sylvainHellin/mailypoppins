//! The read path (`ews.rs`) through the scripted seam: the calendars, a
//! first sync, a sync from the stored state, an invalid state, a series
//! expanded over the window, a server error and a refused login.

use chrono::{TimeZone, Utc};

use super::super::fetch::EwsFetchError;
use super::super::{EwsCalendarBackend, EwsToken};
use super::parse::{CALENDAR_ID, EXCEPTION_ID, OCCURRENCE_ID, SERIES_UID, SINGLE_ID};
use super::ScriptedFetch;
use crate::calendar_backend::{CalendarBackend, CalendarBackendError};
use crate::calendar_model::{window_for, Calendar, CalendarWindow, EventKind};
use crate::config::EwsPasswordSource;

const GET_FOLDER: &str = include_str!("../../../../tests/fixtures/ews/getfolder_calendar.xml");
const FIND_FOLDER: &str = include_str!("../../../../tests/fixtures/ews/findfolder_calendars.xml");
const VIEW: &str = include_str!("../../../../tests/fixtures/ews/finditem_calendarview.xml");
const SINGLE: &str = include_str!("../../../../tests/fixtures/ews/getitem_single_attendees.xml");
const OCCURRENCE: &str = include_str!("../../../../tests/fixtures/ews/getitem_occurrence.xml");
const MASTER: &str = include_str!("../../../../tests/fixtures/ews/getitem_recurring_master.xml");
const EXCEPTION: &str = include_str!("../../../../tests/fixtures/ews/getitem_exception.xml");
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

pub(super) const MASTER_ID: &str = "AQMkVBg8OIjdLZBuHCO0AZHdGCSuMm47YWI4ISGjCFqaAAK1VcIhUZKSMYAZn3ZmApWgCNjC48PZ5BhoHhrSfyE5YiyKz2bDRF35wtkd4MMFUxCTLWAQvhV2IorIq8fIGM52txmt1URP/N8tCLQWQF==";

/// The GetItem answer of an item the server no longer has.
pub(super) const NOT_FOUND: &str = "<m:GetItemResponseMessage ResponseClass=\"Error\">\
    <m:MessageText>The specified object was not found in the store.</m:MessageText>\
    <m:ResponseCode>ErrorItemNotFound</m:ResponseCode>\
    <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey><m:Items/></m:GetItemResponseMessage>";

fn backend(fetch: ScriptedFetch) -> EwsCalendarBackend<ScriptedFetch> {
    EwsCalendarBackend::with_fetch("tum", EwsPasswordSource::Imap, fetch)
}

pub(super) fn calendar() -> Calendar {
    Calendar {
        account: "tum".into(),
        id: CALENDAR_ID.into(),
        name: "Calendar".into(),
        color: None,
        is_default: true,
        can_edit: true,
        visible: true,
        online_meeting_providers: Vec::new(),
    }
}

/// 2026-10-01 to 2027-11-01, which holds every fixture item.
fn window() -> CalendarWindow {
    window_for(Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap())
}

/// The text between `open` and `close` in `text`.
fn between<'a>(text: &'a str, open: &str, close: &str) -> &'a str {
    let start = text.find(open).expect("the opening") + open.len();
    &text[start..start + text[start..].find(close).expect("the closing")]
}

fn state_of(page: &str) -> String {
    between(page, "<m:SyncState>", "</m:SyncState>").to_string()
}

/// One GetItem answer with a response message per part, in order: the
/// message of a GetItem fixture, or a message given as it is.
pub(super) fn get_item(parts: &[&str]) -> String {
    let messages: String = parts
        .iter()
        .map(|part| {
            if part.starts_with("<m:GetItemResponseMessage") {
                part.to_string()
            } else {
                let open = part.find("<m:GetItemResponseMessage").unwrap();
                let close = "</m:GetItemResponseMessage>";
                let end = part.find(close).unwrap() + close.len();
                part[open..end].to_string()
            }
        })
        .collect();
    let head = SINGLE.find("<m:ResponseMessages>").unwrap() + "<m:ResponseMessages>".len();
    let tail = SINGLE.find("</m:ResponseMessages>").unwrap();
    format!("{}{messages}{}", &SINGLE[..head], &SINGLE[tail..])
}

/// A sync page with `changes` as its `Changes`.
fn page_with(changes: &str) -> String {
    FINAL_EMPTY.replace("<m:Changes/>", &format!("<m:Changes>{changes}</m:Changes>"))
}

fn token(state: &str) -> String {
    EwsToken {
        sync_state: state.to_string(),
    }
    .encode()
}

/// The script of a baseline from nothing: the two state pages, the view,
/// its items, and the masters of its two series (the exception's master
/// gone).
fn baseline_script(fetch: ScriptedFetch) -> ScriptedFetch {
    fetch
        .answer("SyncFolderItems", FIRST_PAGE)
        .answer("SyncFolderItems", LAST_PAGE)
        .answer("FindItem", VIEW)
        .answer("GetItem", &get_item(&[SINGLE, OCCURRENCE, EXCEPTION]))
        .answer("GetItem", &get_item(&[MASTER, NOT_FOUND]))
}

/// GetFolder names the default calendar and FindFolder every calendar,
/// listed once, the default first.
#[tokio::test]
async fn the_calendars_are_the_appointment_folders_the_default_marked() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("GetFolder", GET_FOLDER)
            .answer("FindFolder", FIND_FOLDER),
    );
    let calendars = backend.list_calendars().await.unwrap();
    assert_eq!(calendars, [calendar()]);
    let sent = backend.fetch().sent();
    assert!(sent[0].contains("<t:DistinguishedFolderId Id=\"calendar\"/>"));
    assert!(sent[1].contains("<m:FindFolder Traversal=\"Deep\">"));
    assert!(sent[1].contains("<t:Constant Value=\"IPF.Appointment\"/>"));
    assert!(sent[1].contains("<t:DistinguishedFolderId Id=\"msgfolderroot\"/>"));
}

/// A first sync takes the state from nothing over both pages, then reads
/// the window's view and its items, and each series' master once: the
/// occurrence hangs from its master and says its rule, the exception whose
/// master is gone keeps what it has. The result replaces the window.
#[tokio::test]
async fn a_first_sync_reads_the_state_then_the_window_and_its_series() {
    let backend = backend(baseline_script(ScriptedFetch::default()));
    let result = backend
        .sync_window(&calendar(), window(), None)
        .await
        .unwrap();
    assert!(result.full_resync);
    assert_eq!(result.path.as_deref(), Some("sync-folder-items"));
    assert_eq!(result.token, Some(token(&state_of(LAST_PAGE))));
    let ids: Vec<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, [SINGLE_ID, OCCURRENCE_ID, EXCEPTION_ID]);
    let kinds: Vec<EventKind> = result.upserts.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::Single,
            EventKind::Occurrence,
            EventKind::Exception
        ]
    );
    let occurrence = &result.upserts[1];
    assert_eq!(occurrence.uid, SERIES_UID);
    assert_eq!(occurrence.series_master_id.as_deref(), Some(MASTER_ID));
    assert_eq!(
        occurrence.recurrence.as_deref(),
        Some("Weekly on Wednesday until 2027-10-06")
    );
    assert_eq!(result.upserts[2].series_master_id, None);
    assert_eq!(result.series.len(), 1);
    assert_eq!(result.series[0].master_id, MASTER_ID);
    assert_eq!(result.series[0].uid, SERIES_UID);
    assert_eq!(result.series[0].zone.as_deref(), Some("Europe/Berlin"));
    assert!(result.series[0].recurrence.is_some());
    assert!(result.removed.is_empty() && result.resent_series.is_empty());

    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 5);
    assert!(
        !sent[0].contains("<m:SyncState>"),
        "the first page starts from nothing"
    );
    assert!(sent[0].contains(&format!(
        "<m:SyncFolderId><t:FolderId Id=\"{CALENDAR_ID}\"/>"
    )));
    assert!(sent[0].contains("<m:MaxChangesReturned>512</m:MaxChangesReturned>"));
    assert!(sent[1].contains(&format!(
        "<m:SyncState>{}</m:SyncState>",
        state_of(FIRST_PAGE)
    )));
    assert!(sent[2].contains(
        "<m:CalendarView StartDate=\"2026-10-01T00:00:00Z\" EndDate=\"2027-11-01T00:00:00Z\"/>"
    ));
    for id in [SINGLE_ID, OCCURRENCE_ID, EXCEPTION_ID] {
        assert!(sent[3].contains(&format!("<t:ItemId Id=\"{id}\"/>")));
    }
    assert!(
        sent[3].contains("<t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType>")
    );
    assert!(sent[4].contains(&format!(
        "<t:RecurringMasterItemId OccurrenceId=\"{OCCURRENCE_ID}\"/>\
         <t:RecurringMasterItemId OccurrenceId=\"{EXCEPTION_ID}\"/>"
    )));
    assert!(backend.fetch().exhausted());
}

/// A sync from the stored state that finds nothing changed asks once and
/// hands back the server's next state.
#[tokio::test]
async fn a_second_sync_passes_the_stored_state_and_brings_nothing_new() {
    let stored = state_of(LAST_PAGE);
    let backend = backend(ScriptedFetch::default().answer("SyncFolderItems", FINAL_EMPTY));
    let result = backend
        .sync_window(&calendar(), window(), Some(token(&stored)))
        .await
        .unwrap();
    assert!(!result.full_resync);
    assert!(result.upserts.is_empty() && result.removed.is_empty());
    assert_eq!(result.token, Some(token(&state_of(FINAL_EMPTY))));
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(&format!("<m:SyncState>{stored}</m:SyncState>")));
}

/// A sync from the stored state reads a changed single event again, removes
/// a deleted item, and reads a changed series with its occurrences in the
/// window, which the store holds the series to.
#[tokio::test]
async fn a_second_sync_applies_the_changes_and_resends_a_changed_series() {
    let changes = page_with(&format!(
        "<t:Update><t:CalendarItem><t:ItemId Id=\"{SINGLE_ID}\" ChangeKey=\"k\"/>\
         <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem></t:Update>\
         <t:Delete><t:ItemId Id=\"gone\" ChangeKey=\"k\"/></t:Delete>\
         <t:Update><t:CalendarItem><t:ItemId Id=\"{MASTER_ID}\" ChangeKey=\"k\"/>\
         <t:CalendarItemType>RecurringMaster</t:CalendarItemType></t:CalendarItem></t:Update>"
    ));
    let backend = backend(
        ScriptedFetch::default()
            .answer("SyncFolderItems", &changes)
            .answer("GetItem", &get_item(&[SINGLE, MASTER]))
            .answer("FindItem", VIEW)
            .answer("GetItem", &get_item(&[OCCURRENCE])),
    );
    let result = backend
        .sync_window(&calendar(), window(), Some(token("stored")))
        .await
        .unwrap();
    assert!(!result.full_resync);
    let ids: Vec<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, [SINGLE_ID, OCCURRENCE_ID]);
    assert_eq!(result.removed, ["gone", MASTER_ID]);
    assert_eq!(result.resent_series, [SINGLE_ID, MASTER_ID]);
    assert_eq!(result.series.len(), 1);
    assert_eq!(
        result.upserts[1].series_master_id.as_deref(),
        Some(MASTER_ID)
    );
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 4);
    assert!(sent[3].contains(&format!("<t:ItemId Id=\"{OCCURRENCE_ID}\"/>")));
    assert!(
        !sent[3].contains(EXCEPTION_ID),
        "only the changed series' occurrences are read"
    );
    assert!(backend.fetch().exhausted());
}

/// The CalendarView fixture cut to its first item, the single event.
fn view_of_first_item() -> String {
    let second = VIEW.match_indices("<t:CalendarItem>").nth(1).unwrap().0;
    let end = VIEW.find("</t:Items>").unwrap();
    format!("{}{}", &VIEW[..second], &VIEW[end..])
}

/// A store holding the baseline `result` of the calendar, and the sync
/// after it applied.
fn store_after(
    baseline: &crate::calendar_backend::SyncResult,
    incremental: &crate::calendar_backend::SyncResult,
) -> (tempfile::TempDir, crate::store::Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(dir.path().join("store.sqlite3")).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 11, 13, 8, 0, 0).unwrap();
    for result in [baseline, incremental] {
        crate::store::calendar::apply_sync(&store, "tum", CALENDAR_ID, result, window(), now)
            .unwrap();
    }
    (dir, store)
}

/// The ids of the events the store holds for the account, sorted.
fn stored_ids(store: &crate::store::Store) -> Vec<String> {
    let mut ids: Vec<String> =
        crate::store::calendar::events_in_range(store, &["tum"], window(), chrono_tz::UTC, false)
            .unwrap()
            .into_iter()
            .map(|event| event.id)
            .collect();
    ids.sort();
    ids
}

/// A single event that became a series under the same ItemId leaves no row
/// of its own: the series master is held with the series, and the cache
/// shows its occurrences in the window instead.
#[tokio::test]
async fn a_single_event_that_became_a_series_leaves_no_stale_row() {
    let was_single = SINGLE.replace(SINGLE_ID, MASTER_ID);
    let first = backend(
        ScriptedFetch::default()
            .answer("SyncFolderItems", LAST_PAGE)
            .answer(
                "FindItem",
                &view_of_first_item().replace(SINGLE_ID, MASTER_ID),
            )
            .answer("GetItem", &get_item(&[&was_single])),
    );
    let first_view = first
        .sync_window(&calendar(), window(), None)
        .await
        .unwrap();
    let changes = page_with(&format!(
        "<t:Update><t:CalendarItem><t:ItemId Id=\"{MASTER_ID}\" ChangeKey=\"k\"/>\
         <t:CalendarItemType>RecurringMaster</t:CalendarItemType></t:CalendarItem></t:Update>"
    ));
    let second = backend(
        ScriptedFetch::default()
            .answer("SyncFolderItems", &changes)
            .answer("GetItem", &get_item(&[MASTER]))
            .answer("FindItem", VIEW)
            .answer("GetItem", &get_item(&[OCCURRENCE])),
    );
    let second_view = second
        .sync_window(&calendar(), window(), Some(token("stored")))
        .await
        .unwrap();
    assert!(second.fetch().exhausted());
    let (_dir, store) = store_after(&first_view, &second_view);
    assert_eq!(stored_ids(&store), [OCCURRENCE_ID]);
    let series = crate::store::calendar::get_series(&store, "tum", MASTER_ID).unwrap();
    assert_eq!(series.map(|series| series.uid).as_deref(), Some(SERIES_UID));
}

/// A series that became a single event under the same ItemId takes its
/// cached occurrences along: the single event stands alone, and an
/// exception of another series stays.
#[tokio::test]
async fn a_series_that_became_a_single_event_leaves_no_stale_occurrence() {
    let first = backend(baseline_script(ScriptedFetch::default()));
    let first_view = first
        .sync_window(&calendar(), window(), None)
        .await
        .unwrap();
    let changes = page_with(&format!(
        "<t:Update><t:CalendarItem><t:ItemId Id=\"{MASTER_ID}\" ChangeKey=\"k\"/>\
         <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem></t:Update>"
    ));
    let now_single = SINGLE.replace(SINGLE_ID, MASTER_ID);
    let second = backend(
        ScriptedFetch::default()
            .answer("SyncFolderItems", &changes)
            .answer("GetItem", &get_item(&[&now_single])),
    );
    let second_view = second
        .sync_window(&calendar(), window(), Some(token("stored")))
        .await
        .unwrap();
    assert!(second.fetch().exhausted());
    let (_dir, store) = store_after(&first_view, &second_view);
    let mut expected = vec![SINGLE_ID, MASTER_ID, EXCEPTION_ID];
    expected.sort();
    assert_eq!(stored_ids(&store), expected);
    let single = crate::store::calendar::get_event(&store, "tum", MASTER_ID)
        .unwrap()
        .unwrap();
    assert_eq!(single.kind, EventKind::Single);
}

/// A single event moved out of the window, or gone before it was read, is
/// removed from it.
#[tokio::test]
async fn an_event_that_left_the_window_is_removed() {
    let changes = page_with(&format!(
        "<t:Update><t:CalendarItem><t:ItemId Id=\"{SINGLE_ID}\" ChangeKey=\"k\"/>\
         <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem></t:Update>\
         <t:Create><t:CalendarItem><t:ItemId Id=\"brief\" ChangeKey=\"k\"/>\
         <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem></t:Create>"
    ));
    let moved = SINGLE
        .replace(
            "<t:Start>2026-11-13T07:30:00Z",
            "<t:Start>2028-11-13T07:30:00Z",
        )
        .replace("<t:End>2026-11-13T09:30:00Z", "<t:End>2028-11-13T09:30:00Z");
    let backend = backend(
        ScriptedFetch::default()
            .answer("SyncFolderItems", &changes)
            .answer("GetItem", &get_item(&[&moved, NOT_FOUND])),
    );
    let result = backend
        .sync_window(&calendar(), window(), Some(token("stored")))
        .await
        .unwrap();
    assert!(result.upserts.is_empty());
    assert_eq!(result.removed, [SINGLE_ID, "brief"]);
}

/// A state the server no longer knows starts a baseline from nothing,
/// which replaces the window.
#[tokio::test]
async fn an_invalid_sync_state_falls_back_to_a_full_sync() {
    let backend = backend(baseline_script(
        ScriptedFetch::default().answer("SyncFolderItems", INVALID_STATE),
    ));
    let result = backend
        .sync_window(&calendar(), window(), Some(token("stale")))
        .await
        .unwrap();
    assert!(result.full_resync);
    assert_eq!(result.upserts.len(), 3);
    assert_eq!(result.token, Some(token(&state_of(LAST_PAGE))));
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 6);
    assert!(sent[0].contains("<m:SyncState>stale</m:SyncState>"));
    assert!(!sent[1].contains("<m:SyncState>"));
    assert!(backend.fetch().exhausted());
}

/// A token this build cannot read is no token: the sync is a baseline.
#[tokio::test]
async fn an_unreadable_token_is_a_baseline() {
    let backend = backend(baseline_script(ScriptedFetch::default()));
    let result = backend
        .sync_window(
            &calendar(),
            window(),
            Some("{\"path\":\"calendar-delta\"}".into()),
        )
        .await
        .unwrap();
    assert!(result.full_resync);
    assert!(!backend.fetch().sent()[0].contains("<m:SyncState>"));
}

/// A view the server cuts at its limit is read again in two halves, an item
/// both halves name kept once.
#[tokio::test]
async fn a_cut_calendar_view_is_read_in_halves() {
    let cut = VIEW.replace(
        "IncludesLastItemInRange=\"true\"",
        "IncludesLastItemInRange=\"false\"",
    );
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &cut)
            .answer("FindItem", VIEW)
            .answer("FindItem", VIEW),
    );
    let items = backend.view(CALENDAR_ID, window()).await.unwrap();
    assert_eq!(items.len(), 3);
    let sent = backend.fetch().sent();
    assert!(sent[1].contains("StartDate=\"2026-10-01T00:00:00Z\" EndDate=\"2027-04-17T00:00:00Z\""));
    assert!(sent[2].contains("StartDate=\"2027-04-17T00:00:00Z\" EndDate=\"2027-11-01T00:00:00Z\""));
}

/// A server error reaches the daemon as one sentence naming the operation
/// and the server's code: `server_error` shows the error's top line, which
/// is the whole of it.
#[tokio::test]
async fn a_server_error_is_the_daemons_sentence() {
    let busy = INVALID_STATE
        .replace(
            "Synchronization state data is corrupt or otherwise invalid.",
            "The server cannot service this request right now. Try again later.",
        )
        .replace("ErrorInvalidSyncStateData", "ErrorServerBusy");
    let backend = backend(
        ScriptedFetch::default()
            .answer("SyncFolderItems", &busy)
            .answer("SyncFolderItems", VERSION_REFUSED)
            .fail(
                "GetFolder",
                EwsFetchError::Status {
                    status: 503,
                    body: "Service Unavailable".into(),
                },
            ),
    );
    let top_line = |err: CalendarBackendError| match err {
        CalendarBackendError::Other(err) => format!("{err}"),
        other => panic!("not a server error: {other:?}"),
    };
    let err = backend
        .sync_window(&calendar(), window(), Some(token("stored")))
        .await
        .unwrap_err();
    assert_eq!(
        top_line(err),
        "the Exchange server refused SyncFolderItems: ErrorServerBusy: The server cannot \
         service this request right now. Try again later."
    );
    let err = backend
        .sync_window(&calendar(), window(), None)
        .await
        .unwrap_err();
    assert_eq!(
        top_line(err),
        "the Exchange server refused SyncFolderItems: ErrorInvalidRequest: The request is invalid."
    );
    let err = backend.list_calendars().await.unwrap_err();
    assert_eq!(
        top_line(err),
        "the Exchange server answered GetFolder with HTTP 503"
    );
}

/// A refused login stops the account's pass at once: the calendar after it
/// is not synced, the server is asked nothing more, and the error names the
/// command that stores the password again.
#[tokio::test]
async fn a_refused_login_stops_the_pass_and_is_never_retried() {
    let second = FIND_FOLDER.replace(
        "</t:CalendarFolder></t:Folders>",
        "</t:CalendarFolder><t:CalendarFolder><t:FolderId Id=\"second\" ChangeKey=\"k\"/>\
         <t:FolderClass>IPF.Appointment</t:FolderClass><t:DisplayName>Second</t:DisplayName>\
         </t:CalendarFolder></t:Folders>",
    );
    let backend = backend(
        ScriptedFetch::default()
            .answer("GetFolder", GET_FOLDER)
            .answer("FindFolder", &second)
            .fail(
                "SyncFolderItems",
                EwsFetchError::LoginRefused("HTTP 401 after the NTLM handshake".into()),
            ),
    );
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(dir.path().join("store.sqlite3")).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 11, 13, 8, 0, 0).unwrap();
    let err = crate::calendar_sync::sync_account(&store, &backend, "tum", now)
        .await
        .unwrap_err();
    let expected = format!(
        "the Exchange server refused the EWS login (HTTP 401 after the NTLM handshake); \
         set the password again with `{}`",
        mp_core::mp_command::here(&["config", "set-password", "imap", "--account", "tum"])
    );
    assert!(
        matches!(err, CalendarBackendError::LoginRefused(ref sentence) if *sentence == expected),
        "{err:?}"
    );
    assert_eq!(err.to_string(), expected);
    assert_eq!(backend.fetch().sent().len(), 3, "nothing after the refusal");
    assert!(backend.fetch().exhausted());

    // The backend lives for one pass; anything else asked of it in that pass
    // answers the same refusal without asking the server.
    let again = backend.list_calendars().await.unwrap_err();
    assert!(matches!(again, CalendarBackendError::LoginRefused(_)));
    assert_eq!(backend.fetch().sent().len(), 3);
}

/// The backend serves every feature beyond reading and answering but an
/// online meeting, which on-premises Exchange cannot make.
#[test]
fn the_backend_serves_every_feature_but_an_online_meeting() {
    use crate::calendar_model::CalendarFeature;
    assert_eq!(
        super::super::FEATURES,
        [
            CalendarFeature::Write,
            CalendarFeature::ProposeTime,
            CalendarFeature::FreeBusy,
            CalendarFeature::ServerReminders,
        ]
    );
}
