//! The Graph writes (PERSO-255): every URL and body Graph receives, the
//! answers converted, and the immutable-id header on all four verbs.

use serde_json::json;

use super::*;
use crate::calendar_backend::Written;
use crate::calendar_model::{AttendeeRole, Sensitivity, ShowAs};
use crate::calendar_write::{AttendeeInput, EventInput, EventPatch};

/// What Graph answers a create or an update of a single event with.
fn created(id: &str) -> String {
    json!({
        "id": id,
        "iCalUId": "UID-NEW",
        "type": "singleInstance",
        "subject": "Project review",
        "start": {"dateTime": "2026-11-03T09:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-03T10:00:00.0000000", "timeZone": "UTC"},
        "isAllDay": false,
        "originalStartTimeZone": "W. Europe Standard Time",
        "isOrganizer": true,
        "responseStatus": {"response": "organizer"},
        "isReminderOn": true,
        "reminderMinutesBeforeStart": 10,
    })
    .to_string()
}

fn berlin(start_hour: u32, end_hour: u32) -> EventTime {
    EventTime::timed(utc(3, start_hour, 0), utc(3, end_hour, 0), "Europe/Berlin").unwrap()
}

fn written_event(written: Written) -> Event {
    match written {
        Written::Event(event) => *event,
        Written::Series(series) => panic!("expected an event, got the series {series:?}"),
    }
}

/// A create with every field posts each in Graph's terms, the times as
/// Berlin wall time under its Windows name, and converts the answer.
#[tokio::test]
async fn a_create_posts_every_field_with_the_transaction_id() {
    // Graph answers a Teams create with the meeting, its flags and the
    // transaction id, which the conversion carries into the event.
    let mut answer: serde_json::Value = serde_json::from_str(&created("AAMk/new=")).unwrap();
    for (key, value) in [
        ("isOnlineMeeting", json!(true)),
        ("onlineMeetingProvider", json!("teamsForBusiness")),
        (
            "onlineMeeting",
            json!({"joinUrl": "https://teams.microsoft.com/l/meetup-join/new"}),
        ),
        ("allowNewTimeProposals", json!(false)),
        ("responseRequested", json!(true)),
        ("transactionId", json!("tx-1")),
    ] {
        answer[key] = value;
    }
    let fetch = ScriptedFetch::default().route(
        &format!("{BASE}/me/calendars/"),
        status(201, &answer.to_string()),
    );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let mut input = EventInput::new("Project review", berlin(9, 10));
    input.location = Some("Room 1".into());
    input.body = Some("Agenda: budget".into());
    input.attendees = vec![
        AttendeeInput {
            email: "ada@example.com".into(),
            name: Some("Ada Lovelace".into()),
            role: AttendeeRole::Required,
        },
        AttendeeInput {
            email: "bob@example.com".into(),
            name: None,
            role: AttendeeRole::Optional,
        },
        AttendeeInput {
            email: "room-1@example.com".into(),
            name: Some(" ".into()),
            role: AttendeeRole::Resource,
        },
    ];
    input.online_meeting = true;
    input.reminder = Some(Some(10));
    input.show_as = Some(ShowAs::WorkingElsewhere);
    input.sensitivity = Some(Sensitivity::Private);
    input.allow_new_time_proposals = Some(false);

    let cal = calendar("AAMk/cal=", true);
    let written = backend.create_event(&cal, &input, "tx-1").await.unwrap();
    let event = written_event(written);
    assert_eq!(event.id, "AAMk/new=");
    assert_eq!(event.calendar, "AAMk/cal=");
    assert_eq!(event.uid, "UID-NEW");
    assert_eq!(event.time, berlin(9, 10));
    assert_eq!(event.reminder, Some(10));
    assert_eq!(event.kind, EventKind::Single);
    assert!(event.is_online_meeting);
    assert_eq!(
        event.online_meeting_provider,
        Some(OnlineMeetingProvider::TeamsForBusiness)
    );
    assert_eq!(
        event.online_meeting_url.as_deref(),
        Some("https://teams.microsoft.com/l/meetup-join/new")
    );
    assert!(!event.allow_new_time_proposals);
    assert_eq!(event.transaction_id.as_deref(), Some("tx-1"));

    let sent = backend.fetch.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(
        sent[0].url,
        format!("{BASE}/me/calendars/AAMk%2Fcal=/events")
    );
    assert_eq!(
        sent[0].body,
        Some(json!({
            "subject": "Project review",
            "start": {"dateTime": "2026-11-03T10:00:00", "timeZone": "W. Europe Standard Time"},
            "end": {"dateTime": "2026-11-03T11:00:00", "timeZone": "W. Europe Standard Time"},
            "isAllDay": false,
            "transactionId": "tx-1",
            "location": {"displayName": "Room 1"},
            "body": {"contentType": "text", "content": "Agenda: budget"},
            "attendees": [
                {"emailAddress": {"address": "ada@example.com", "name": "Ada Lovelace"}, "type": "required"},
                {"emailAddress": {"address": "bob@example.com"}, "type": "optional"},
                {"emailAddress": {"address": "room-1@example.com"}, "type": "resource"},
            ],
            "isOnlineMeeting": true,
            "onlineMeetingProvider": "teamsForBusiness",
            "isReminderOn": true,
            "reminderMinutesBeforeStart": 10,
            "showAs": "workingElsewhere",
            "sensitivity": "private",
            "allowNewTimeProposals": false,
        }))
    );
}

/// A create that leaves a field to the server sends nothing for it: the
/// mailbox default reminder, no Teams link, Graph's show-as and
/// sensitivity; a `null` reminder turns it off.
#[tokio::test]
async fn a_minimal_create_leaves_the_defaults_to_the_server() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/calendars/"), status(201, &created("a")))
        .route(&format!("{BASE}/me/calendars/"), status(201, &created("b")));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let cal = calendar("cal", true);
    let input = EventInput::new("Focus", berlin(9, 10));
    backend.create_event(&cal, &input, "tx-a").await.unwrap();
    let mut silent = input.clone();
    silent.reminder = Some(None);
    backend.create_event(&cal, &silent, "tx-b").await.unwrap();

    let sent = backend.fetch.sent();
    let times = json!({
        "start": {"dateTime": "2026-11-03T10:00:00", "timeZone": "W. Europe Standard Time"},
        "end": {"dateTime": "2026-11-03T11:00:00", "timeZone": "W. Europe Standard Time"},
    });
    let mut expected = json!({"subject": "Focus", "isAllDay": false, "transactionId": "tx-a"});
    expected["start"] = times["start"].clone();
    expected["end"] = times["end"].clone();
    assert_eq!(sent[0].body, Some(expected.clone()));
    expected["transactionId"] = "tx-b".into();
    expected["isReminderOn"] = false.into();
    assert_eq!(sent[1].body, Some(expected));
}

/// A regional zone (Vienna) and the current spelling of one CLDR names the
/// legacy way (Kolkata) go out as their wall time under their Windows name;
/// a zone the CLDR table has no Windows name for goes out as UTC with the
/// instants exact, as does a UTC event; an all-day event goes out as its
/// midnights in its zone, the end exclusive, and in UTC for a zone without a
/// Windows name.
#[tokio::test]
async fn times_go_out_in_their_windows_zone_or_in_utc() {
    let fetch = (0..6).fold(ScriptedFetch::default(), |fetch, _| {
        fetch.route(&format!("{BASE}/me/calendars/"), status(201, &created("x")))
    });
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let cal = calendar("cal", true);
    let timed = |zone| EventTime::timed(utc(3, 9, 30), utc(3, 10, 0), zone).unwrap();
    let times = [
        timed("Antarctica/Troll"),
        timed("UTC"),
        EventTime::all_day(day(5), day(7)).unwrap(),
        timed("Europe/Vienna"),
        timed("Asia/Kolkata"),
    ];
    for (i, time) in times.into_iter().enumerate() {
        let mut input = EventInput::new("Slot", time);
        input.zone = Some("Europe/Berlin".into());
        backend
            .create_event(&cal, &input, &format!("tx-{i}"))
            .await
            .unwrap();
    }
    let mut troll = EventInput::new("Slot", EventTime::all_day(day(5), day(7)).unwrap());
    troll.zone = Some("Antarctica/Troll".into());
    backend.create_event(&cal, &troll, "tx-5").await.unwrap();
    let sent = backend.fetch.sent();
    let utc_pair = (
        json!({"dateTime": "2026-11-03T09:30:00", "timeZone": "UTC"}),
        json!({"dateTime": "2026-11-03T10:00:00", "timeZone": "UTC"}),
    );
    for request in &sent[..2] {
        let body = request.body.as_ref().unwrap();
        assert_eq!((body["start"].clone(), body["end"].clone()), utc_pair);
        assert_eq!(body["isAllDay"], false);
    }
    let body = sent[2].body.as_ref().unwrap();
    assert_eq!(
        body["start"],
        json!({"dateTime": "2026-11-05T00:00:00", "timeZone": "W. Europe Standard Time"})
    );
    assert_eq!(
        body["end"],
        json!({"dateTime": "2026-11-07T00:00:00", "timeZone": "W. Europe Standard Time"})
    );
    assert_eq!(body["isAllDay"], true);
    let body = sent[5].body.as_ref().unwrap();
    assert_eq!(
        (body["start"].clone(), body["end"].clone()),
        (
            json!({"dateTime": "2026-11-05T00:00:00", "timeZone": "UTC"}),
            json!({"dateTime": "2026-11-07T00:00:00", "timeZone": "UTC"}),
        )
    );
    let zoned = |request: &Sent| {
        let body = request.body.as_ref().unwrap();
        (body["start"].clone(), body["end"].clone())
    };
    assert_eq!(
        zoned(&sent[3]),
        (
            json!({"dateTime": "2026-11-03T10:30:00", "timeZone": "W. Europe Standard Time"}),
            json!({"dateTime": "2026-11-03T11:00:00", "timeZone": "W. Europe Standard Time"}),
        )
    );
    assert_eq!(
        zoned(&sent[4]),
        (
            json!({"dateTime": "2026-11-03T15:00:00", "timeZone": "India Standard Time"}),
            json!({"dateTime": "2026-11-03T15:30:00", "timeZone": "India Standard Time"}),
        )
    );
}

/// A patch sends only what it names: an empty one is `{}`, a cleared
/// location and description are empty, a cleared reminder is off, a new
/// time comes with its all-day flag, turning Teams on names Teams as the
/// provider, and turning it off sends nothing, since Graph keeps the link.
#[tokio::test]
async fn an_update_patches_only_the_fields_it_names() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/events/"), ok(&created("AAMk/ev=")))
        .route(&format!("{BASE}/me/events/"), ok(&created("AAMk/ev=")))
        .route(&format!("{BASE}/me/events/"), ok(&created("AAMk/ev=")))
        .route(&format!("{BASE}/me/events/"), ok(&created("AAMk/ev=")));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);

    let event = written_event(
        backend
            .update_event("cal", "AAMk/ev=", &EventPatch::default())
            .await
            .unwrap(),
    );
    assert_eq!(
        (event.id.as_str(), event.calendar.as_str()),
        ("AAMk/ev=", "cal")
    );

    let cleared = EventPatch {
        location: Some(None),
        body: Some(None),
        reminder: Some(None),
        online_meeting: Some(false),
        ..Default::default()
    };
    backend
        .update_event("cal", "AAMk/ev=", &cleared)
        .await
        .unwrap();

    let moved = EventPatch {
        summary: Some("Project review, moved".into()),
        time: Some(EventTime::all_day(day(4), day(5)).unwrap()),
        zone: Some("America/New_York".into()),
        attendees: Some(Vec::new()),
        reminder: Some(Some(30)),
        show_as: Some(ShowAs::Free),
        sensitivity: Some(Sensitivity::Normal),
        allow_new_time_proposals: Some(true),
        ..Default::default()
    };
    backend
        .update_event("cal", "AAMk/ev=", &moved)
        .await
        .unwrap();

    let teams = EventPatch {
        online_meeting: Some(true),
        ..Default::default()
    };
    backend
        .update_event("cal", "AAMk/ev=", &teams)
        .await
        .unwrap();

    let sent = backend.fetch.sent();
    assert!(sent.iter().all(|s| s.method == "PATCH"));
    assert!(sent
        .iter()
        .all(|s| s.url == format!("{BASE}/me/events/AAMk%2Fev=")));
    assert_eq!(sent[0].body, Some(json!({})));
    assert_eq!(
        sent[1].body,
        Some(json!({
            "location": {"displayName": ""},
            "body": {"contentType": "text", "content": ""},
            "isReminderOn": false,
        }))
    );
    assert_eq!(
        sent[2].body,
        Some(json!({
            "subject": "Project review, moved",
            "start": {"dateTime": "2026-11-04T00:00:00", "timeZone": "Eastern Standard Time"},
            "end": {"dateTime": "2026-11-05T00:00:00", "timeZone": "Eastern Standard Time"},
            "isAllDay": true,
            "attendees": [],
            "isReminderOn": true,
            "reminderMinutesBeforeStart": 30,
            "showAs": "free",
            "sensitivity": "normal",
            "allowNewTimeProposals": true,
        }))
    );
    assert_eq!(
        sent[3].body,
        Some(json!({"isOnlineMeeting": true, "onlineMeetingProvider": "teamsForBusiness"}))
    );
}

/// A create replayed with its transaction id after its body changed meets
/// Graph's `ErrorDuplicateTransactionId`: the create adopts the event the
/// first one made, patches it with every field it sets and the server's
/// defaults for the ones it leaves out, and answers it as the created event.
/// A 400 of any other code stays a refusal.
#[tokio::test]
async fn a_changed_replay_of_a_transaction_patches_the_event_it_made() {
    let duplicate = json!({"error": {
        "code": "ErrorDuplicateTransactionId",
        "message": "The TransactionId specified in the request has already been used to create a different event.",
        "innerError": {"code": "ClientIdAlreadyInUseException"},
        "@event.existingEventId": "AAMk/first=",
    }});
    let fetch = ScriptedFetch::default()
        .route(
            &format!("{BASE}/me/calendars/"),
            status(400, &duplicate.to_string()),
        )
        .route(&format!("{BASE}/me/events/"), ok(&created("AAMk/first=")))
        .route(
            &format!("{BASE}/me/calendars/"),
            status(400, r#"{"error":{"code":"ErrorInvalidRequest"}}"#),
        );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let cal = calendar("cal", true);
    let mut input = EventInput::new("Project review, later", berlin(10, 11));
    input.location = Some("Room 2".into());
    input.attendees = vec![AttendeeInput {
        email: "ada@example.com".into(),
        name: None,
        role: AttendeeRole::Required,
    }];
    input.reminder = Some(None);

    let event = written_event(backend.create_event(&cal, &input, "tx-1").await.unwrap());
    assert_eq!(event.id, "AAMk/first=");
    assert_eq!(event.calendar, "cal");

    let sent = backend.fetch.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[1].method, "PATCH");
    assert_eq!(sent[1].url, format!("{BASE}/me/events/AAMk%2Ffirst="));
    let mut expected = sent[0].body.clone().unwrap();
    let fields = expected.as_object_mut().unwrap();
    fields.remove("transactionId");
    for (key, value) in [
        ("body", json!({"contentType": "text", "content": ""})),
        ("showAs", json!("busy")),
        ("sensitivity", json!("normal")),
        ("allowNewTimeProposals", json!(true)),
    ] {
        fields.insert(key.into(), value);
    }
    assert_eq!(sent[1].body, Some(expected));
    assert_eq!(
        sent[1].body.as_ref().unwrap()["location"],
        json!({"displayName": "Room 2"})
    );

    let err = backend
        .create_event(&cal, &input, "tx-2")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Graph refused the new event: HTTP 400: ErrorInvalidRequest"
    );
}

/// The patch a replay sends clears what the replay left out, in Graph's
/// terms: an empty location display name, an empty plain-text body, an empty
/// attendee list (which uninvites everyone), the reminder off for a replay
/// with none, and the server's defaults for show-as, sensitivity and new
/// time proposals. A reminder left to the mailbox default and an online
/// meeting the replay does not ask for stay off the patch.
#[test]
fn a_replay_patch_clears_what_the_replay_left_out() {
    let input = EventInput::new("Project review", berlin(9, 10));
    let mut sent = super::super::write::patch_body(&input.as_patch());
    let fields = sent.as_object_mut().unwrap();
    for key in ["subject", "start", "end", "isAllDay"] {
        assert!(fields.remove(key).is_some(), "{key}");
    }
    assert_eq!(
        sent,
        json!({
            "location": {"displayName": ""},
            "body": {"contentType": "text", "content": ""},
            "attendees": [],
            "showAs": "busy",
            "sensitivity": "normal",
            "allowNewTimeProposals": true,
        })
    );

    let mut no_reminder = input;
    no_reminder.reminder = Some(None);
    let sent = super::super::write::patch_body(&no_reminder.as_patch());
    assert_eq!(sent["isReminderOn"], json!(false));
    assert!(sent.get("reminderMinutesBeforeStart").is_none());
}

fn exception_answer() -> String {
    json!({
        "id": "AAMkEv-weekly-occ",
        "iCalUId": "OWN-OCCURRENCE-UID",
        "type": "exception",
        "seriesMasterId": "AAMkEv-weekly",
        "subject": "Weekly sync, moved",
        "start": {"dateTime": "2026-11-10T14:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-10T15:00:00.0000000", "timeZone": "UTC"},
        "isAllDay": false,
        "originalStart": "2026-11-10T13:00:00Z",
        "originalStartTimeZone": "W. Europe Standard Time",
    })
    .to_string()
}

/// An occurrence written on its own comes back with the series' `UID`, read
/// from its master once and then from what the backend knows.
#[tokio::test]
async fn an_updated_occurrence_takes_its_series_uid() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/events/"), ok(&exception_answer()))
        .route(&format!("{BASE}/me/events/"), ok(&exception_answer()))
        .route(
            &format!("{BASE}/me/calendars/AAMkCal-default/events/AAMkEv-weekly?"),
            ok(MASTER),
        );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let patch = EventPatch {
        summary: Some("Weekly sync, moved".into()),
        ..Default::default()
    };
    for _ in 0..2 {
        let event = written_event(
            backend
                .update_event("AAMkCal-default", "AAMkEv-weekly-occ", &patch)
                .await
                .unwrap(),
        );
        assert_eq!(event.uid, MASTER_UID);
        assert_eq!(event.series_master_id.as_deref(), Some("AAMkEv-weekly"));
        assert_eq!(event.recurrence_id.as_deref(), Some("2026-11-10T13:00:00Z"));
        assert_eq!(event.summary, "Weekly sync, moved");
    }
    let gets = backend
        .fetch
        .sent()
        .iter()
        .filter(|s| s.method == "GET")
        .count();
    assert_eq!(gets, 1, "the master is read once");
}

/// A write to a whole series answers with its master, which comes back as
/// the series, rule included.
#[tokio::test]
async fn an_updated_series_comes_back_as_its_series() {
    let master = json!({
        "id": "AAMkEv-weekly",
        "iCalUId": MASTER_UID,
        "type": "seriesMaster",
        "subject": "Weekly sync, renamed",
        "start": {"dateTime": "2026-11-03T13:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-03T14:00:00.0000000", "timeZone": "UTC"},
        "isAllDay": false,
        "originalStartTimeZone": "W. Europe Standard Time",
        "recurrence": {
            "pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["tuesday"]},
            "range": {"type": "noEnd", "startDate": "2026-11-03"},
        },
    });
    let fetch =
        ScriptedFetch::default().route(&format!("{BASE}/me/events/"), ok(&master.to_string()));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let patch = EventPatch {
        summary: Some("Weekly sync, renamed".into()),
        ..Default::default()
    };
    let written = backend
        .update_event("AAMkCal-default", "AAMkEv-weekly", &patch)
        .await
        .unwrap();
    let Written::Series(series) = written else {
        panic!("expected the series, got {written:?}");
    };
    assert_eq!(series.master_id, "AAMkEv-weekly");
    assert_eq!(series.uid, MASTER_UID);
    assert_eq!(
        series.recurrence.clone().map(|rule| Recurrence {
            start: None,
            ..rule
        }),
        Recurrence::from_rrule("FREQ=WEEKLY;BYDAY=TU", chrono_tz::UTC)
    );
    let master = series.master.as_deref().unwrap();
    assert_eq!(master.kind, EventKind::SeriesMaster);
    assert_eq!(master.recurrence.as_deref(), Some("Weekly on Tuesday"));
    assert_eq!(series.zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(
        series.master.as_ref().map(|m| m.summary.as_str()),
        Some("Weekly sync, renamed")
    );
    assert!(backend.known_series("AAMkEv-weekly").is_some());
}

/// A refused create or update carries Graph's code, which the daemon words,
/// and an update of an event Graph no longer has fails: the change did not
/// land.
#[tokio::test]
async fn a_refused_write_reports_the_graph_code() {
    let crossing = r#"{"error":{"code":"ErrorOccurrenceCrossingBoundary","message":"nope"}}"#;
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/events/"), status(400, crossing))
        .route(&format!("{BASE}/me/events/"), status(404, "{}"))
        .route(
            &format!("{BASE}/me/calendars/"),
            status(403, r#"{"error":{"code":"ErrorAccessDenied"}}"#),
        );
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    let patch = EventPatch {
        time: Some(berlin(9, 10)),
        ..Default::default()
    };
    let err = backend
        .update_event("cal", "occ", &patch)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Graph refused the update: HTTP 400: ErrorOccurrenceCrossingBoundary: nope"
    );
    let err = backend
        .update_event("cal", "gone", &patch)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Graph refused the update: HTTP 404");
    let input = EventInput::new("Review", berlin(9, 10));
    let err = backend
        .create_event(&calendar("shared", false), &input, "tx")
        .await
        .unwrap_err();
    assert!(matches!(err, CalendarBackendError::Other(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "Graph refused the new event: HTTP 403: ErrorAccessDenied"
    );
}

/// A cancel posts its comment, a blank one none; a delete sends no body;
/// either of an event Graph no longer has is done, and a refusal is not.
#[tokio::test]
async fn cancel_and_delete_name_the_event_and_count_a_gone_one_as_done() {
    let refusal = r#"{"error":{"code":"ErrorInvalidRequest","message":"Your request can't be completed. You need to be an organizer to cancel a meeting."}}"#;
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/events/"), status(202, ""))
        .route(&format!("{BASE}/me/events/"), status(202, ""))
        .route(&format!("{BASE}/me/events/"), status(404, "{}"))
        .route(&format!("{BASE}/me/events/"), status(400, refusal))
        .route(&format!("{BASE}/me/events/"), status(204, ""))
        .route(&format!("{BASE}/me/events/"), status(404, "{}"));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    backend
        .cancel_event("AAMk/ev=", Some("Moved to next week".into()))
        .await
        .unwrap();
    backend
        .cancel_event("AAMk/ev=", Some("  ".into()))
        .await
        .unwrap();
    backend.cancel_event("gone", None).await.unwrap();
    let err = backend.cancel_event("theirs", None).await.unwrap_err();
    assert!(
        err.to_string()
            .starts_with("Graph refused the cancel: HTTP 400: ErrorInvalidRequest"),
        "{err}"
    );
    backend.delete_event("AAMk/ev=").await.unwrap();
    backend.delete_event("gone").await.unwrap();

    let sent = backend.fetch.sent();
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, format!("{BASE}/me/events/AAMk%2Fev=/cancel"));
    assert_eq!(sent[0].body, Some(json!({"comment": "Moved to next week"})));
    assert_eq!(sent[1].body, Some(json!({})));
    assert_eq!(sent[2].url, format!("{BASE}/me/events/gone/cancel"));
    assert_eq!(sent[4].method, "DELETE");
    assert_eq!(sent[4].url, format!("{BASE}/me/events/AAMk%2Fev="));
    assert_eq!(sent[4].body, None);
    assert_eq!(sent[5].url, format!("{BASE}/me/events/gone"));
}

/// A dismiss posts `{}` (an empty POST is HTTP 411 at Exchange), a snooze the new reminder time in UTC; a gone
/// event has no reminder left, which counts as done.
#[tokio::test]
async fn reminders_are_dismissed_and_snoozed_on_the_server() {
    let fetch = ScriptedFetch::default()
        .route(&format!("{BASE}/me/events/"), status(200, ""))
        .route(&format!("{BASE}/me/events/"), status(200, ""))
        .route(&format!("{BASE}/me/events/"), status(404, "{}"))
        .route(&format!("{BASE}/me/events/"), status(500, "{}"));
    let backend = GraphCalendarBackend::with_fetch("evoqs", fetch);
    backend.dismiss_reminder("AAMk/ev=").await.unwrap();
    backend
        .snooze_reminder("AAMk/ev=", utc(3, 8, 55))
        .await
        .unwrap();
    backend.dismiss_reminder("gone").await.unwrap();
    let err = backend
        .snooze_reminder("AAMk/ev=", utc(3, 8, 55))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Graph refused the reminder snooze: HTTP 500"
    );

    let sent = backend.fetch.sent();
    assert_eq!(sent[0].method, "POST");
    assert_eq!(
        sent[0].url,
        format!("{BASE}/me/events/AAMk%2Fev=/dismissReminder")
    );
    assert_eq!(sent[0].body, Some(json!({})));
    assert_eq!(
        sent[1].url,
        format!("{BASE}/me/events/AAMk%2Fev=/snoozeReminder")
    );
    assert_eq!(
        sent[1].body,
        Some(json!({"newReminderTime": {"dateTime": "2026-11-03T08:55:00", "timeZone": "UTC"}}))
    );
}

/// The production seam asks for immutable ids on all four verbs, sends the
/// token, and posts JSON with its length, an empty object included, which
/// is what `dismissReminder` takes.
#[test]
fn every_verb_of_the_seam_asks_for_immutable_ids() {
    let http = GraphHttp {
        client: crate::graph::GraphClient::for_tests(),
    };
    let url = format!("{BASE}/me/events/x");
    let page = [format!("odata.maxpagesize={PAGE_SIZE}")];
    let requests = [
        ("GET", http.build(reqwest::Method::GET, &url, &page, None)),
        (
            "POST",
            http.build(reqwest::Method::POST, &url, &[], Some(json!({"a": 1}))),
        ),
        (
            "PATCH",
            http.build(reqwest::Method::PATCH, &url, &[], Some(json!({"a": 1}))),
        ),
        (
            "DELETE",
            http.build(reqwest::Method::DELETE, &url, &[], None),
        ),
        (
            "POST (empty object)",
            http.build(reqwest::Method::POST, &url, &[], Some(json!({}))),
        ),
    ];
    for (what, request) in requests {
        let request = request.build().unwrap();
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .map(|v| v.to_str().unwrap().to_string())
        };
        let prefer = header("Prefer").unwrap_or_default();
        assert!(
            prefer.starts_with(r#"IdType="ImmutableId""#),
            "{what}: {prefer}"
        );
        assert_eq!(
            header("Authorization").as_deref(),
            Some("Bearer token"),
            "{what}"
        );
        let body = request.body().and_then(|b| b.as_bytes());
        match what {
            "GET" => {
                assert_eq!(prefer, r#"IdType="ImmutableId", odata.maxpagesize=100"#);
                assert!(body.is_none());
            }
            "DELETE" => assert!(body.is_none(), "{what}"),
            "POST (empty object)" => {
                assert_eq!(body, Some(&b"{}"[..]));
                assert_eq!(header("Content-Type").as_deref(), Some("application/json"));
            }
            _ => {
                assert_eq!(body, Some(&br#"{"a":1}"#[..]), "{what}");
                assert_eq!(header("Content-Type").as_deref(), Some("application/json"));
            }
        }
    }
}
