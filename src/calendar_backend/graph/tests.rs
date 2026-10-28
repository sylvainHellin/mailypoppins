//! The Graph calendar adapter against hand-written Graph answers, through the
//! [`GraphFetch`] seam: no tenant, no mock crate.

use std::collections::VecDeque;

use chrono::NaiveDate;

use super::*;
use crate::calendar_model::{window_for, EventKey};

const CALENDARS: &str = include_str!("../../../tests/fixtures/graph_calendar/calendars.json");
const PAGE1: &str = include_str!("../../../tests/fixtures/graph_calendar/delta_page1.json");
const PAGE2: &str = include_str!("../../../tests/fixtures/graph_calendar/delta_page2.json");
const MASTER: &str = include_str!("../../../tests/fixtures/graph_calendar/series_master.json");

const MASTER_UID: &str = "040000008200E00074C5B7101A82E00800000000WEEKLY";
const BASE: &str = "https://graph.microsoft.com/v1.0";

/// One request the adapter made.
#[derive(Debug, Clone)]
struct Sent {
    method: &'static str,
    url: String,
    prefer: String,
    body: Option<serde_json::Value>,
}

/// A Graph that answers from a script: each route is a URL prefix with the
/// answers it gives in turn. A request no route matches panics, so a test
/// names every call it expects.
#[derive(Default)]
struct ScriptedFetch {
    routes: Mutex<Vec<(String, VecDeque<HttpAnswer>)>>,
    sent: Mutex<Vec<Sent>>,
}

impl ScriptedFetch {
    fn route(self, prefix: &str, answer: HttpAnswer) -> Self {
        {
            let mut routes = self.routes.lock().unwrap();
            match routes.iter_mut().find(|(p, _)| p == prefix) {
                Some((_, answers)) => answers.push_back(answer),
                None => routes.push((prefix.to_string(), VecDeque::from([answer]))),
            }
        }
        self
    }

    fn answer(
        &self,
        method: &'static str,
        url: &str,
        prefer: String,
        body: Option<serde_json::Value>,
    ) -> HttpAnswer {
        self.sent.lock().unwrap().push(Sent {
            method,
            url: url.to_string(),
            prefer,
            body,
        });
        let mut routes = self.routes.lock().unwrap();
        // The longest matching prefix wins, so a specific route shadows a
        // general one.
        let route = routes
            .iter_mut()
            .filter(|(prefix, answers)| url.starts_with(prefix.as_str()) && !answers.is_empty())
            .max_by_key(|(prefix, _)| prefix.len())
            .unwrap_or_else(|| panic!("no scripted answer for {method} {url}"));
        route.1.pop_front().unwrap()
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }
}

impl GraphFetch for ScriptedFetch {
    async fn get(&self, url: &str, prefer: &[String]) -> anyhow::Result<HttpAnswer> {
        Ok(self.answer("GET", url, prefer_header(prefer), None))
    }

    async fn post(&self, url: &str, body: serde_json::Value) -> anyhow::Result<HttpAnswer> {
        Ok(self.answer("POST", url, prefer_header(&[]), Some(body)))
    }
}

fn ok(body: &str) -> HttpAnswer {
    HttpAnswer {
        status: 200,
        body: body.to_string(),
        location: None,
    }
}

fn status(status: u16, body: &str) -> HttpAnswer {
    HttpAnswer {
        status,
        body: body.to_string(),
        location: None,
    }
}

fn window() -> CalendarWindow {
    window_for(Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap())
}

const RANGE: &str = "startDateTime=2026-10-01T00:00:00Z&endDateTime=2027-11-01T00:00:00Z";

fn calendar(id: &str, is_default: bool) -> Calendar {
    Calendar {
        account: "evoqs".into(),
        id: id.into(),
        name: id.into(),
        color: None,
        is_default,
        can_edit: true,
        visible: true,
    }
}

fn utc(d: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap()
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

fn find<'a>(result: &'a SyncResult, id: &str) -> &'a Event {
    result
        .upserts
        .iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("no event {id} in {:?}", result.upserts))
}

fn delta_url(calendar: &str) -> String {
    format!("{BASE}/me/calendars/{calendar}/calendarView/delta?{RANGE}")
}

/// The default calendar's baseline over the two fixture pages.
fn baseline_fetch() -> ScriptedFetch {
    ScriptedFetch::default()
        .route(&delta_url("AAMkCal-default"), ok(PAGE1))
        .route(
            &format!("{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$skiptoken=page2"),
            ok(PAGE2),
        )
        .route(&format!("{BASE}/me/events/AAMkEv-weekly?"), ok(MASTER))
}

#[tokio::test]
async fn the_calendar_list_maps_colours_flags_and_permissions() {
    let fetch = ScriptedFetch::default().route(&format!("{BASE}/me/calendars"), ok(CALENDARS));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let calendars = backend.list_calendars().await.unwrap();
    let summary: Vec<_> = calendars
        .iter()
        .map(|c| {
            (
                c.id.as_str(),
                c.color.as_deref(),
                c.is_default,
                c.can_edit,
                c.visible,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("AAMkCal-default", None, true, true, true),
            ("AAMkCal/holidays+1=", Some("#87d28e"), false, false, true),
            ("AAMkCal-team", Some("#ff8c00"), false, true, true),
        ]
    );
    assert!(calendars.iter().all(|c| c.account == "evoqs"));
    let sent = backend.fetch.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].prefer, r#"IdType="ImmutableId""#);
}

#[tokio::test]
async fn a_baseline_follows_next_link_then_delta_link_and_converts_every_kind() {
    let backend = GraphCalendarBackend::with_fetch("evoqs", baseline_fetch());
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), None)
        .await
        .unwrap();

    assert!(result.full_resync);
    assert_eq!(result.path.as_deref(), Some("calendar-delta"));
    let token = GraphToken::decode(result.token.as_deref().unwrap()).unwrap();
    assert_eq!(
        token,
        GraphToken {
            path: SyncPath::CalendarDelta,
            link: Some(format!(
                "{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$deltatoken=round1"
            )),
        }
    );
    assert_eq!(result.removed, ["AAMkEv-gone"]);
    let ids: Vec<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "AAMkEv-single",
            "AAMkEv-allday",
            "AAMkEv-weekly-occ2",
            "AAMkEv-weekly-exc3",
            "AAMkEv-custom"
        ]
    );

    // A timed meeting: UTC instants, the Windows zone mapped, HTML body as text.
    let single = find(&result, "AAMkEv-single");
    assert_eq!(
        single.time,
        EventTime::timed(utc(3, 8, 0), utc(3, 9, 0), "Europe/Berlin").unwrap()
    );
    assert_eq!(single.uid, "040000008200E00074C5B7101A82E00800000000SINGLE");
    assert_eq!(single.recurrence_id, None);
    assert_eq!(single.series_master_id, None);
    assert_eq!(single.calendar, "AAMkCal-default");
    let body = single.body.as_deref().unwrap();
    assert!(
        body.contains("Agenda") && body.contains("budget") && !body.contains('<'),
        "{body}"
    );
    assert_eq!(single.location.as_deref(), Some("Room 1"));
    assert_eq!(single.organizer.as_ref().unwrap().email, "ada@example.com");
    let attendees: Vec<_> = single
        .attendees
        .iter()
        .map(|a| (a.email.as_str(), a.role, a.status))
        .collect();
    assert_eq!(
        attendees,
        [
            (
                "ada@example.com",
                AttendeeRole::Required,
                ResponseStatus::Accepted
            ),
            (
                "sylvain@evoqs.de",
                AttendeeRole::Optional,
                ResponseStatus::NeedsAction
            ),
            (
                "room1@evoqs.de",
                AttendeeRole::Resource,
                ResponseStatus::Tentative
            ),
        ]
    );
    assert_eq!(single.own_response, OwnResponse::NeedsAction);
    assert_eq!(single.reminder, Some(15));
    assert_eq!(
        single.online_meeting_url.as_deref(),
        Some("https://teams.microsoft.com/l/meetup-join/abc")
    );
    assert_eq!(single.show_as, ShowAs::Tentative);
    assert_eq!(
        single.last_modified,
        Some(
            Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap()
                + chrono::Duration::nanoseconds(123_456_700)
        )
    );
    assert!(!single.is_organizer && !single.cancelled);

    // An all-day event on its own dates, the end exclusive.
    let holiday = find(&result, "AAMkEv-allday");
    assert_eq!(holiday.time, EventTime::all_day(day(5), day(7)).unwrap());
    assert_eq!(holiday.time.last_day(), Some(day(6)));
    assert_eq!(holiday.reminder, None, "isReminderOn is false");
    assert_eq!(holiday.sensitivity, Sensitivity::Private);
    assert_eq!(holiday.show_as, ShowAs::Oof);
    assert_eq!(holiday.own_response, OwnResponse::Organizer);
    assert_eq!(holiday.location, None);
    assert_eq!(holiday.body, None);

    // An occurrence and an exception share the series master's UID, and
    // each is keyed by its original start.
    let occurrence = find(&result, "AAMkEv-weekly-occ2");
    assert_eq!(occurrence.uid, MASTER_UID);
    assert_eq!(
        occurrence.recurrence_id.as_deref(),
        Some("2026-11-10T13:00:00Z")
    );
    assert_eq!(
        occurrence.series_master_id.as_deref(),
        Some("AAMkEv-weekly")
    );
    assert_eq!(occurrence.recurrence, None);
    assert_eq!(
        occurrence.dedup_key(),
        EventKey::new(MASTER_UID, Some("2026-11-10T14:00:00+01:00"))
    );
    let exception = find(&result, "AAMkEv-weekly-exc3");
    assert_eq!(exception.uid, MASTER_UID);
    assert_eq!(
        exception.recurrence_id.as_deref(),
        Some("2026-11-17T13:00:00Z")
    );
    assert_eq!(
        exception.time,
        EventTime::timed(utc(18, 15, 0), utc(18, 16, 0), "Europe/Berlin").unwrap()
    );
    assert_ne!(occurrence.dedup_key(), exception.dedup_key());

    // A custom Outlook zone keeps exact instants under a UTC label.
    let custom = find(&result, "AAMkEv-custom");
    assert_eq!(
        custom.time,
        EventTime::timed(utc(20, 9, 30), utc(20, 10, 0), "UTC").unwrap()
    );
    assert_eq!(
        custom.online_meeting_url.as_deref(),
        Some("https://meet.example.com/legacy")
    );
    assert_eq!(custom.sensitivity, Sensitivity::Confidential);
    assert_eq!(custom.show_as, ShowAs::WorkingElsewhere);
    assert_eq!(custom.own_response, OwnResponse::Accepted);
    assert!(custom.cancelled);

    // The master was read once for both instances, and handed to the store.
    assert_eq!(
        result.series,
        [SeriesInfo {
            master_id: "AAMkEv-weekly".into(),
            uid: MASTER_UID.into(),
            recurrence: Some("FREQ=WEEKLY;BYDAY=TU;WKST=MO;UNTIL=20270629".into()),
        }]
    );
    let sent = backend.fetch.sent();
    let urls: Vec<&str> = sent.iter().map(|s| s.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            delta_url("AAMkCal-default").as_str(),
            &format!("{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$skiptoken=page2"),
            &format!(
                "{BASE}/me/events/AAMkEv-weekly?$select=iCalUId,recurrence,originalStartTimeZone"
            ),
        ]
    );
    assert_eq!(
        sent[0].prefer,
        r#"IdType="ImmutableId", odata.maxpagesize=100"#
    );
    assert!(sent
        .iter()
        .all(|s| s.prefer.starts_with(r#"IdType="ImmutableId""#)));
}

#[tokio::test]
async fn an_incremental_sync_follows_the_link_verbatim_and_reuses_known_series() {
    let link = format!("{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$deltatoken=round1");
    let fetch = ScriptedFetch::default().route(&link, ok(PAGE2));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    backend.remember_series(vec![SeriesInfo {
        master_id: "AAMkEv-weekly".into(),
        uid: MASTER_UID.into(),
        recurrence: None,
    }]);
    let token = GraphToken {
        path: SyncPath::CalendarDelta,
        link: Some(link.clone()),
    }
    .encode();
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), Some(token))
        .await
        .unwrap();
    assert!(!result.full_resync);
    assert!(result.series.is_empty(), "a known master is not read again");
    assert_eq!(find(&result, "AAMkEv-weekly-exc3").uid, MASTER_UID);
    assert_eq!(result.removed, ["AAMkEv-gone"]);
    let sent = backend.fetch.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, link);
}

#[tokio::test]
async fn a_410_drops_the_token_and_syncs_the_window_afresh() {
    let stale = format!("{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$deltatoken=old");
    let gone = HttpAnswer {
        status: 410,
        body: r#"{"error":{"code":"resyncRequired","message":"Resync required."}}"#.into(),
        location: Some(format!(
            "{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$deltatoken="
        )),
    };
    let fetch = baseline_fetch().route(&stale, gone);
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let token = GraphToken {
        path: SyncPath::CalendarDelta,
        link: Some(stale.clone()),
    }
    .encode();
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), Some(token))
        .await
        .unwrap();
    assert!(result.full_resync);
    assert_eq!(result.upserts.len(), 5);
    let sent = backend.fetch.sent();
    assert_eq!(sent[0].url, stale);
    assert_eq!(sent[1].url, delta_url("AAMkCal-default"));
}

#[tokio::test]
async fn a_sync_state_not_found_code_is_a_reset_too() {
    let stale = format!("{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$deltatoken=old");
    let fetch = baseline_fetch().route(
        &stale,
        status(
            400,
            r#"{"error":{"code":"syncStateNotFound","message":"The sync state was not found."}}"#,
        ),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let token = GraphToken {
        path: SyncPath::CalendarDelta,
        link: Some(stale),
    }
    .encode();
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), Some(token))
        .await
        .unwrap();
    assert!(result.full_resync);
    assert_eq!(result.path.as_deref(), Some("calendar-delta"));
}

#[tokio::test]
async fn a_default_calendar_without_its_own_delta_falls_back_to_the_documented_one() {
    let default_delta = format!("{BASE}/me/calendarView/delta?{RANGE}");
    let fetch = ScriptedFetch::default()
        .route(
            &delta_url("AAMkCal-default"),
            status(
                400,
                r#"{"error":{"code":"BadRequest","message":"Unsupported."}}"#,
            ),
        )
        .route(&default_delta, ok(PAGE2))
        .route(&format!("{BASE}/me/events/AAMkEv-weekly?"), ok(MASTER));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), None)
        .await
        .unwrap();
    assert!(result.full_resync);
    assert_eq!(result.path.as_deref(), Some("default-delta"));
    let token = GraphToken::decode(result.token.as_deref().unwrap()).unwrap();
    assert_eq!(token.path, SyncPath::DefaultDelta);
    assert!(token.link.unwrap().ends_with("$deltatoken=round1"));
    assert_eq!(backend.fetch.sent()[1].url, default_delta);
}

#[tokio::test]
async fn another_calendar_without_its_own_delta_is_read_in_full_every_sync() {
    let id = "AAMkCal%2Fholidays%2B1=";
    let view = format!("{BASE}/me/calendars/{id}/calendarView?{RANGE}");
    let view_page = PAGE2.replace("@odata.deltaLink", "@odata.unusedLink");
    let fetch = ScriptedFetch::default()
        .route(
            &delta_url(id),
            status(404, r#"{"error":{"code":"ResourceNotFound"}}"#),
        )
        .route(&view, ok(&view_page))
        .route(&view, ok(&view_page))
        .route(&format!("{BASE}/me/events/AAMkEv-weekly?"), ok(MASTER))
        .route(&format!("{BASE}/me/events/AAMkEv-weekly?"), ok(MASTER));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let holidays = calendar("AAMkCal/holidays+1=", false);
    let first = backend
        .sync_window(&holidays, window(), None)
        .await
        .unwrap();
    assert!(first.full_resync);
    assert_eq!(first.path.as_deref(), Some("calendar-view"));
    let token = GraphToken::decode(first.token.as_deref().unwrap()).unwrap();
    assert_eq!(
        token,
        GraphToken {
            path: SyncPath::CalendarView,
            link: None
        }
    );
    assert!(find(&first, "AAMkEv-weekly-exc3").calendar == "AAMkCal/holidays+1=");

    // The marker token skips the failing probe: straight to the view.
    let second = backend
        .sync_window(&holidays, window(), first.token.clone())
        .await
        .unwrap();
    assert!(second.full_resync);
    let urls: Vec<String> = backend.fetch.sent().into_iter().map(|s| s.url).collect();
    assert_eq!(urls[0], delta_url(id));
    assert_eq!(urls[1], view);
    assert_eq!(urls.iter().filter(|u| u.contains("/delta")).count(), 1);
    assert_eq!(urls.iter().filter(|u| **u == view).count(), 2);
}

#[tokio::test]
async fn a_forbidden_calendar_list_is_a_missing_scope() {
    let fetch = ScriptedFetch::default().route(
        &format!("{BASE}/me/calendars"),
        status(
            403,
            r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#,
        ),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let err = backend.list_calendars().await.unwrap_err();
    assert!(matches!(
        err,
        CalendarBackendError::MissingScope { ref scope } if scope == "Calendars.ReadWrite"
    ));
}

#[tokio::test]
async fn another_failure_is_reported_and_leaves_the_token_alone() {
    let link = format!("{BASE}/me/calendars/AAMkCal-default/calendarView/delta?$deltatoken=round1");
    let fetch = ScriptedFetch::default().route(&link, status(503, "{}"));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let token = GraphToken {
        path: SyncPath::CalendarDelta,
        link: Some(link),
    }
    .encode();
    let err = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), Some(token))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 503"), "{err}");
}

#[tokio::test]
async fn a_forbidden_calendar_window_fails_that_calendar_alone() {
    // A shared calendar its owner's ACL refuses answers 403 on its window:
    // an ordinary failure the sync logs before the next calendar, not a
    // missing scope that would stop the account with a re-login prompt.
    let link = format!("{BASE}/me/calendars/AAMkCal-shared/calendarView/delta?$deltatoken=round1");
    let fetch = ScriptedFetch::default().route(
        &link,
        status(
            403,
            r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#,
        ),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let token = GraphToken {
        path: SyncPath::CalendarDelta,
        link: Some(link),
    }
    .encode();
    let err = backend
        .sync_window(&calendar("AAMkCal-shared", false), window(), Some(token))
        .await
        .unwrap_err();
    assert!(matches!(err, CalendarBackendError::Other(_)), "{err:?}");
    assert!(err.to_string().contains("HTTP 403"), "{err}");
}

#[tokio::test]
async fn respond_posts_the_answer_with_its_comment_and_send_choice() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/events/"), status(202, ""))
        .route(&format!("{BASE}/me/events/"), status(202, ""));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    backend
        .respond("AAMk/ev=", Response::Tentative, Some("Late".into()), false)
        .await
        .unwrap();
    backend
        .respond("AAMk/ev=", Response::Decline, None, true)
        .await
        .unwrap();
    let sent = backend.fetch.sent();
    assert_eq!(sent[0].method, "POST");
    assert_eq!(
        sent[0].url,
        format!("{BASE}/me/events/AAMk%2Fev=/tentativelyAccept")
    );
    assert_eq!(
        sent[0].body,
        Some(serde_json::json!({"comment": "Late", "sendResponse": false}))
    );
    assert_eq!(sent[1].url, format!("{BASE}/me/events/AAMk%2Fev=/decline"));
    assert_eq!(
        sent[1].body,
        Some(serde_json::json!({"sendResponse": true}))
    );
}

/// An `eventMessage` names its event, a plain message names none, and a
/// message the server no longer has stands for nothing; every lookup asks for
/// immutable ids.
#[tokio::test]
async fn an_invitation_mail_resolves_to_its_server_event() {
    let fetch = ScriptedFetch::default()
        .route(
            &format!("{BASE}/me/messages/"),
            ok(r#"{"id":"AAMk/msg=","event":{"id":"AAMk/ev="}}"#),
        )
        .route(&format!("{BASE}/me/messages/"), ok(r#"{"id":"plain"}"#))
        .route(&format!("{BASE}/me/messages/"), status(404, "{}"));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    assert_eq!(
        backend.event_for_message("AAMk/msg=").await.unwrap(),
        Some("AAMk/ev=".to_string())
    );
    assert_eq!(backend.event_for_message("plain").await.unwrap(), None);
    assert_eq!(backend.event_for_message("gone").await.unwrap(), None);
    let sent = backend.fetch.sent();
    assert_eq!(
        sent[0].url,
        format!(
            "{BASE}/me/messages/AAMk%2Fmsg=?$select=id\
             &$expand=microsoft.graph.eventMessage/event($select=id)"
        )
    );
    assert!(sent[0].prefer.contains("ImmutableId"), "{}", sent[0].prefer);
}

#[tokio::test]
async fn the_last_word_on_an_id_wins_within_one_sync() {
    let page = serde_json::json!({
        "@odata.deltaLink": format!("{BASE}/next"),
        "value": [
            serde_json::from_str::<serde_json::Value>(PAGE2).unwrap()["value"][2].clone(),
            {"id": "AAMkEv-custom", "@removed": {"reason": "deleted"}},
            {"id": "AAMkEv-back", "@removed": {"reason": "deleted"}},
            {
                "id": "AAMkEv-back",
                "iCalUId": "back",
                "type": "singleInstance",
                "start": {"dateTime": "2026-11-21T09:00:00", "timeZone": "UTC"},
                "end": {"dateTime": "2026-11-21T10:00:00", "timeZone": "UTC"}
            }
        ]
    });
    let fetch =
        ScriptedFetch::default().route(&delta_url("AAMkCal-default"), ok(&page.to_string()));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), None)
        .await
        .unwrap();
    assert_eq!(result.removed, ["AAMkEv-custom"]);
    let ids: Vec<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["AAMkEv-back"]);
    // No originalStartTimeZone: labelled UTC.
    assert_eq!(result.upserts[0].time.zone(), Some(chrono_tz::UTC));
}

#[tokio::test]
async fn a_series_master_in_a_page_answers_the_lookup_and_is_not_shown() {
    let mut master: serde_json::Value = serde_json::from_str(MASTER).unwrap();
    master["type"] = "seriesMaster".into();
    let occurrence = serde_json::from_str::<serde_json::Value>(PAGE1).unwrap()["value"][2].clone();
    let page = serde_json::json!({
        "@odata.deltaLink": format!("{BASE}/next"),
        "value": [master, occurrence],
    });
    let fetch =
        ScriptedFetch::default().route(&delta_url("AAMkCal-default"), ok(&page.to_string()));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let result = backend
        .sync_window(&calendar("AAMkCal-default", true), window(), None)
        .await
        .unwrap();
    let ids: Vec<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["AAMkEv-weekly-occ2"]);
    assert_eq!(result.upserts[0].uid, MASTER_UID);
    assert_eq!(result.series.len(), 1);
    assert_eq!(backend.fetch.sent().len(), 1, "no master lookup");
}

#[test]
fn an_all_day_event_answered_in_utc_is_read_back_into_its_own_zone() {
    let event: wire::Event = serde_json::from_value(serde_json::json!({
        "id": "x",
        "iCalUId": "x-uid",
        "type": "singleInstance",
        "isAllDay": true,
        "start": {"dateTime": "2026-11-04T23:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-05T23:00:00.0000000", "timeZone": "UTC"},
        "originalStartTimeZone": "W. Europe Standard Time"
    }))
    .unwrap();
    let converted = from_graph("evoqs", "cal", &event, None).unwrap();
    assert_eq!(converted.time, EventTime::all_day(day(5), day(6)).unwrap());
}

#[test]
fn an_all_day_occurrence_is_keyed_by_its_original_date() {
    let event: wire::Event = serde_json::from_value(serde_json::json!({
        "id": "occ",
        "iCalUId": "occ-own-uid",
        "type": "occurrence",
        "seriesMasterId": "master",
        "isAllDay": true,
        "originalStart": "2026-11-09T23:00:00Z",
        "originalStartTimeZone": "W. Europe Standard Time",
        "start": {"dateTime": "2026-11-10T00:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-11T00:00:00.0000000", "timeZone": "UTC"}
    }))
    .unwrap();
    let series = SeriesInfo {
        master_id: "master".into(),
        uid: "series-uid".into(),
        recurrence: None,
    };
    let converted = from_graph("evoqs", "cal", &event, Some(&series)).unwrap();
    assert_eq!(converted.uid, "series-uid");
    assert_eq!(converted.recurrence_id.as_deref(), Some("2026-11-10"));
    // Without the master, the occurrence keeps its own iCalUId.
    let alone = from_graph("evoqs", "cal", &event, None).unwrap();
    assert_eq!(alone.uid, "occ-own-uid");
}

#[test]
fn recurrence_patterns_become_rrules() {
    let rule = |value: serde_json::Value| {
        rrule_of(&serde_json::from_value::<wire::Recurrence>(value).unwrap())
    };
    assert_eq!(
        rule(serde_json::json!({
            "pattern": {"type": "relativeMonthly", "interval": 2, "daysOfWeek": ["thursday"], "index": "last"},
            "range": {"type": "numbered", "numberOfOccurrences": 6}
        }))
        .as_deref(),
        Some("FREQ=MONTHLY;INTERVAL=2;BYDAY=TH;BYSETPOS=-1;COUNT=6")
    );
    assert_eq!(
        rule(serde_json::json!({
            "pattern": {"type": "absoluteYearly", "interval": 1, "month": 3, "dayOfMonth": 14},
            "range": {"type": "noEnd"}
        }))
        .as_deref(),
        Some("FREQ=YEARLY;BYMONTHDAY=14;BYMONTH=3")
    );
    assert_eq!(
        rule(serde_json::json!({"pattern": {"type": "daily", "interval": 1}})).as_deref(),
        Some("FREQ=DAILY")
    );
    assert_eq!(
        rule(serde_json::json!({"pattern": {"type": "hourly"}})),
        None
    );
}

#[test]
fn a_token_round_trips_and_garbage_is_no_token() {
    let token = GraphToken {
        path: SyncPath::DefaultDelta,
        link: Some("https://example.com/delta?$deltatoken=a".into()),
    };
    assert_eq!(GraphToken::decode(&token.encode()), Some(token));
    assert_eq!(GraphToken::decode("fake-calendar-token:0:0"), None);
}
