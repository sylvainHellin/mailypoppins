//! Free/busy (PERSO-255): the `getSchedule` request Graph receives, and its
//! answer read from a fixture in the shape the EVOQS tenant gave live.

use serde_json::json;

use super::*;
use crate::calendar_freebusy::{FreeBusyQuery, Schedule};

const GET_SCHEDULE: &str =
    include_str!("../../../../tests/fixtures/graph_calendar/get_schedule.json");

/// The Berlin day of 4 November 2026 at 30-minute slots, the account first.
fn query() -> FreeBusyQuery {
    FreeBusyQuery {
        addresses: vec![
            "s.hellin@evoqs.com".into(),
            "ada@evoqs.com".into(),
            "someone@gmail.com".into(),
        ],
        window: CalendarWindow {
            start: Utc.with_ymd_and_hms(2026, 11, 3, 23, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 11, 4, 23, 0, 0).unwrap(),
        },
        interval: 30,
    }
}

/// The query goes out as one POST to `getSchedule` with the addresses, the
/// range as UTC wall times and the slot length, under the immutable-id
/// header every request carries.
#[tokio::test]
async fn a_free_busy_query_posts_the_addresses_the_range_and_the_interval() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/calendar/getSchedule"), ok(GET_SCHEDULE));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    backend.free_busy(&query()).await.unwrap();
    let sent = backend.fetch.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, format!("{BASE}/me/calendar/getSchedule"));
    assert_eq!(sent[0].prefer, PREFER_IMMUTABLE_ID);
    assert_eq!(
        sent[0].body,
        Some(json!({
            "schedules": ["s.hellin@evoqs.com", "ada@evoqs.com", "someone@gmail.com"],
            "startTime": {"dateTime": "2026-11-03T23:00:00", "timeZone": "UTC"},
            "endTime": {"dateTime": "2026-11-04T23:00:00", "timeZone": "UTC"},
            "availabilityViewInterval": 30,
        }))
    );
}

/// Each schedule is matched to its address whatever the case Graph answers
/// it in, carries Graph's view and its items in start order with an empty
/// subject or location dropped, and an address Graph cannot read has its
/// error while the others stand.
#[tokio::test]
async fn the_answer_reads_one_schedule_per_address_an_outside_one_its_error() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/calendar/getSchedule"), ok(GET_SCHEDULE));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let schedules = backend.free_busy(&query()).await.unwrap();
    let addresses: Vec<&str> = schedules.iter().map(|s| s.address.as_str()).collect();
    assert_eq!(
        addresses,
        ["s.hellin@evoqs.com", "ada@evoqs.com", "someone@gmail.com"]
    );

    let own = &schedules[0];
    assert_eq!(own.view.len(), 48);
    assert_eq!(&own.view[18..22], "2222");
    assert_eq!(own.error, None);
    assert_eq!(own.items.len(), 1);
    assert_eq!(own.items[0].start, utc(4, 8, 0));
    assert_eq!(own.items[0].end, utc(4, 10, 0));
    assert_eq!(own.items[0].status, ShowAs::Busy);
    assert_eq!(own.items[0].subject.as_deref(), Some("Team meeting"));
    assert_eq!(own.items[0].location, None, "an empty location is none");

    let ada = &schedules[1];
    assert_eq!(&ada.view[26..32], "113333");
    let statuses: Vec<ShowAs> = ada.items.iter().map(|i| i.status).collect();
    assert_eq!(statuses, [ShowAs::Tentative, ShowAs::Oof], "in start order");
    assert_eq!(ada.items[0].location.as_deref(), Some("Room 2"));
    assert_eq!(ada.items[1].subject, None, "a private block has no subject");

    assert_eq!(
        schedules[2],
        Schedule::unavailable("someone@gmail.com", "Proxy web request failed (5016)")
    );
}

/// An address the answer leaves out has no free/busy, rather than failing
/// the query.
#[tokio::test]
async fn an_address_the_answer_leaves_out_has_no_free_busy() {
    let mut answer: serde_json::Value = serde_json::from_str(GET_SCHEDULE).unwrap();
    answer["value"].as_array_mut().unwrap().truncate(1);
    let fetch = ScriptedFetch::default().route(
        &format!("{BASE}/me/calendar/getSchedule"),
        ok(&answer.to_string()),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let schedules = backend.free_busy(&query()).await.unwrap();
    assert_eq!(schedules.len(), 3);
    assert!(schedules[0].error.is_none());
    assert!(schedules[1].error.is_some());
    assert!(schedules[2].view.is_empty());
}

/// A 403 is the token's scope, which the clients turn into the re-login
/// prompt; any other refusal names Graph's code.
#[tokio::test]
async fn a_refused_query_is_the_scope_or_the_servers_sentence() {
    let fetch = ScriptedFetch::default().route(
        &format!("{BASE}/me/calendar/getSchedule"),
        status(
            403,
            r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#,
        ),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    assert!(matches!(
        backend.free_busy(&query()).await,
        Err(CalendarBackendError::MissingScope { .. })
    ));

    let fetch = ScriptedFetch::default().route(
        &format!("{BASE}/me/calendar/getSchedule"),
        status(
            400,
            r#"{"error":{"code":"ErrorTimeIntervalTooBig","message":"The requested time duration is too long."}}"#,
        ),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let err = backend.free_busy(&query()).await.unwrap_err().to_string();
    assert!(err.contains("ErrorTimeIntervalTooBig"), "{err}");
}
