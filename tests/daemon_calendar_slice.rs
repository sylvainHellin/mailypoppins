//! The server calendar over the daemon (PERSO-200, phase 1).
//!
//! Every row runs a real `mp daemon run` against a sandboxed root with the
//! calendar fake armed through `MAILYPOPPINS_DAEMON_FAKE_CALENDAR`, and talks to
//! it over the socket:
//!
//! - `calendar.list` lists the fake's calendars and each account's virtual
//!   invitation calendar;
//! - `calendar.view` merges the server cache and the invitation mails, keeps
//!   the server copy of an invitation the server also has (the occurrences of
//!   a series invitation included), hides a hidden
//!   calendar, and keeps it hidden across `calendar.rebuild`;
//! - an account without a backend views exactly its iMIP agenda;
//! - `event.respond` reaches the fake with every answer, comment and
//!   send-response choice, refuses a sign-in without the calendar scope with
//!   the re-login prompt before calling it, and answers an invitation mail's
//!   event with an iMIP reply;
//! - a tick syncs the calendar after the mail pass and announces
//!   `calendar.changed`;
//! - `message.invite` names the server event of an invitation mail.
//!
//! The fake's events are dated from today, because the cache holds a window
//! around the real clock (`calendar_model::window_for`).

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Duration as Days;
use serde_json::{json, Value};
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::{RecordedResponse, RecordedWrite, FAKE_CALENDAR_ENV};
use mailypoppins::calendar_backend::Response;
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use support::calendar_fixture::{
    at, day, fake_spec_with, seed, seed_series, series_occurrences, write_log, FAKE_FEATURES, IMIP,
    LOCAL_UID, MEETING_UID, SERIES_UID, SERVER,
};
use support::parity::{socket_path, DaemonFixture};
use support::read_fixture;
use support::send_fixture;

const DEADLINE: Duration = Duration::from_secs(20);
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root, its daemon and the fake's answer log.
struct Slice {
    tmp: TempDir,
    daemon: Option<DaemonFixture>,
}

impl Slice {
    fn start() -> Slice {
        Slice::start_with(false, &[])
    }

    fn start_with(missing_scope: bool, extra: &[(&str, String)]) -> Slice {
        Slice::start_seeded(missing_scope, extra, |_| (), Vec::new())
    }

    /// The fixture with the fake armed from `spec`, built over the answer
    /// log at `log` in the root.
    fn start_spec(spec: impl FnOnce(&Path) -> String) -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let spec = spec(&tmp.path().join("calendar.jsonl"));
        let daemon = DaemonFixture::start_with(tmp.path(), None, &[(FAKE_CALENDAR_ENV, &spec)]);
        Slice {
            tmp,
            daemon: Some(daemon),
        }
    }

    /// The fixture plus what `seed_more` writes, and `events` on the fake
    /// beside its own.
    fn start_seeded(
        missing_scope: bool,
        extra: &[(&str, String)],
        seed_more: impl FnOnce(&Path),
        events: Vec<mailypoppins::calendar_model::Event>,
    ) -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        seed_more(tmp.path());
        let spec = fake_spec_with(&tmp.path().join("calendar.jsonl"), missing_scope, events);
        let mut env: Vec<(&str, &str)> = vec![(FAKE_CALENDAR_ENV, spec.as_str())];
        env.extend(extra.iter().map(|(k, v)| (*k, v.as_str())));
        let daemon = DaemonFixture::start_with(tmp.path(), None, &env);
        Slice {
            tmp,
            daemon: Some(daemon),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn answers(&self) -> Vec<RecordedResponse> {
        match std::fs::read_to_string(self.root().join("calendar.jsonl")) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    async fn connect(&self) -> Connection {
        let mut conn = within("connect", Connection::connect(&socket_path(self.root())))
            .await
            .expect("connect");
        within(
            "initialize",
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.root().to_path_buf(),
                    config_dir: self.root().to_path_buf(),
                },
                &[],
                &[],
            ),
        )
        .await
        .expect("initialize");
        conn
    }
}

impl Drop for Slice {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
}

async fn within<F: std::future::Future<Output = T>, T>(what: &str, future: F) -> T {
    tokio::time::timeout(DEADLINE, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not answer within {DEADLINE:?}"))
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    match within(method, conn.call(method, params)).await {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("{method}: expected a refusal, got {other:?}"),
    }
}

/// Start an operation and wait for its terminal status.
async fn run_operation(conn: &mut Connection, method: &str, params: Value) -> Value {
    let started = call(conn, method, params).await;
    wait_for(conn, method, &started).await
}

/// The terminal status of the operation `started` names.
async fn wait_for(conn: &mut Connection, method: &str, started: &Value) -> Value {
    let id = started["operation_id"].as_str().unwrap().to_string();
    let start = Instant::now();
    loop {
        let status = call(conn, "operation.status", json!({"operation_id": id})).await;
        if matches!(
            status["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            return status;
        }
        assert!(start.elapsed() < SETTLE_DEADLINE, "{method} never settled");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn succeeded(status: &Value) -> &Value {
    assert_eq!(status["state"], "succeeded", "{status}");
    &status["result"]
}

/// `calendar.rebuild`'s resync of the server cache, its `server` member.
async fn rebuild(conn: &mut Connection, account: &str) -> Value {
    let status = run_operation(conn, "calendar.rebuild", json!({"account": account})).await;
    let result = succeeded(&status);
    assert_eq!(result["invites_seen"], 1, "the fold still runs: {result}");
    result["server"].clone()
}

/// A view from yesterday over the next month, in UTC.
fn month() -> Value {
    json!({
        "start": day(-1).to_rfc3339(),
        "end": day(30).to_rfc3339(),
        "zone": "UTC",
    })
}

async fn view(conn: &mut Connection, extra: Value) -> Vec<Value> {
    let mut params = month();
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    let answer = call(conn, "calendar.view", params).await;
    answer["events"].as_array().unwrap().clone()
}

fn ids(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            format!(
                "{}/{}",
                e["account"].as_str().unwrap(),
                e["id"].as_str().unwrap()
            )
        })
        .collect()
}

fn row_of(root: &Path, account: &str) -> i64 {
    read_fixture::store(root, account)
        .conn()
        .query_row("SELECT id FROM messages LIMIT 1", [], |row| row.get(0))
        .unwrap()
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// `calendar.list` lists the fake's calendars once a rebuild synced them, and
/// each account's invitation calendar.
#[tokio::test]
async fn calendar_list_shows_the_fake_calendars_and_the_invitation_calendars() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = rebuild(&mut conn, SERVER).await;
    assert_eq!(result["calendars"], 2, "{result}");
    assert_eq!(result["baselines"], 2, "{result}");
    assert_eq!(result["upserts"], 2, "{result}");

    let listed = call(&mut conn, "calendar.list", json!({})).await;
    let rows: Vec<(String, String, String, bool)> = listed["calendars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["account"].as_str().unwrap().into(),
                c["id"].as_str().unwrap().into(),
                c["backend"].as_str().unwrap().into(),
                c["visible"].as_bool().unwrap(),
            )
        })
        .collect();
    let row = |a: &str, i: &str, b: &str| (a.to_string(), i.to_string(), b.to_string(), true);
    assert_eq!(
        rows,
        [
            row(SERVER, "cal-main", "graph"),
            row(SERVER, "cal-second", "graph"),
            row(SERVER, "imip", "none"),
            row(IMIP, "imip", "none"),
        ]
    );
    assert_eq!(listed["calendars"][0]["name"], "Calendar");
    assert_eq!(listed["calendars"][0]["is_default"], true);
    assert_eq!(listed["calendars"][3]["name"], "Invitations");
    // The backend's features on its calendars, less the online meeting on
    // one that offers no Teams, and none on an invitation one.
    assert_eq!(listed["calendars"][0]["features"], json!(FAKE_FEATURES));
    assert_eq!(listed["calendars"][1]["features"], json!(["write"]));
    assert_eq!(listed["calendars"][2]["features"], json!([]));
    assert_eq!(listed["calendars"][3]["features"], json!([]));

    let one = call(&mut conn, "calendar.list", json!({"account": IMIP})).await;
    assert_eq!(one["calendars"].as_array().unwrap().len(), 1);
}

/// The view merges both accounts, keeps the server copy of the meeting the
/// server account was also invited to by mail, and hides a hidden calendar,
/// across a rebuild too.
#[tokio::test]
async fn calendar_view_merges_dedups_and_hides() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let local = format!("{IMIP}/imip:{}", row_of(slice.root(), IMIP));

    let events = view(&mut conn, json!({})).await;
    assert_eq!(
        ids(&events),
        [
            format!("{SERVER}/ev-meeting"),
            local.clone(),
            format!("{SERVER}/ev-holiday"),
        ]
    );
    let meeting = &events[0];
    assert_eq!(meeting["uid"], MEETING_UID);
    assert_eq!(meeting["calendar"], "cal-main");
    assert!(meeting.get("source").is_none(), "{meeting}");
    assert_eq!(meeting["time"]["kind"], "timed");
    assert_eq!(events[2]["time"]["kind"], "all-day");
    assert_eq!(events[1]["source"]["kind"], "imip");
    assert_eq!(events[1]["uid"], LOCAL_UID);

    // Hidden: gone from the view, back with include_hidden.
    let hidden = call(
        &mut conn,
        "calendar.set_visibility",
        json!({"account": SERVER, "calendar": "cal-second", "visible": false}),
    )
    .await;
    assert_eq!(hidden["visible"], false);
    let events = view(&mut conn, json!({})).await;
    assert_eq!(
        ids(&events),
        [format!("{SERVER}/ev-meeting"), local.clone()]
    );
    let all = view(&mut conn, json!({"include_hidden": true})).await;
    assert_eq!(all.len(), 3);
    let only = view(
        &mut conn,
        json!({"include_hidden": true, "calendars": ["cal-second"]}),
    )
    .await;
    assert_eq!(ids(&only), [format!("{SERVER}/ev-holiday")]);

    // A rebuild drops the cache and keeps the preference.
    rebuild(&mut conn, SERVER).await;
    let events = view(&mut conn, json!({})).await;
    assert_eq!(
        ids(&events),
        [format!("{SERVER}/ev-meeting"), local.clone()]
    );
    let listed = call(&mut conn, "calendar.list", json!({"account": SERVER})).await;
    assert_eq!(listed["calendars"][1]["id"], "cal-second");
    assert_eq!(listed["calendars"][1]["visible"], false);

    // The invitation calendar hides an account's mails the same way.
    call(
        &mut conn,
        "calendar.set_visibility",
        json!({"account": IMIP, "calendar": "imip", "visible": false}),
    )
    .await;
    let events = view(&mut conn, json!({})).await;
    assert_eq!(ids(&events), [format!("{SERVER}/ev-meeting")]);

    let unknown = call_err(
        &mut conn,
        "calendar.set_visibility",
        json!({"account": SERVER, "calendar": "nope", "visible": false}),
    )
    .await;
    assert_eq!(unknown.code, -32602);
    let zone = call_err(
        &mut conn,
        "calendar.view",
        json!({"start": day(0).to_rfc3339(), "end": day(1).to_rfc3339(), "zone": "Mars/Base"}),
    )
    .await;
    assert_eq!(zone.code, -32602);
}

/// A series invitation, which carries no RECURRENCE-ID, gives way to the
/// server's occurrences of its UID: the view shows the two occurrences and
/// no copy of the mail beside them.
#[tokio::test]
async fn a_series_invitation_gives_way_to_the_servers_occurrences() {
    let slice = Slice::start_seeded(false, &[], seed_series, series_occurrences());
    let mut conn = slice.connect().await;
    let status = run_operation(&mut conn, "calendar.rebuild", json!({"account": SERVER})).await;
    let server = &succeeded(&status)["server"];
    assert_eq!(server["upserts"], 4, "{server}");

    let events = view(&mut conn, json!({"accounts": [SERVER]})).await;
    assert_eq!(
        ids(&events),
        [
            format!("{SERVER}/ev-meeting"),
            format!("{SERVER}/ev-holiday"),
            format!("{SERVER}/ev-series-1"),
            format!("{SERVER}/ev-series-2"),
        ]
    );
    let series: Vec<&Value> = events.iter().filter(|e| e["uid"] == SERIES_UID).collect();
    assert_eq!(series.len(), 2);
    assert!(series.iter().all(|e| e.get("source").is_none()));
    let first = series[0];
    assert_eq!(first["kind"], "occurrence");
    assert_eq!(first["recurrence"], "Weekly (2 times)");
    assert_eq!(first["is_online_meeting"], true);
    assert_eq!(first["online_meeting_provider"], "teams-for-business");
    assert_eq!(first["allow_new_time_proposals"], true);
    assert_eq!(events[0]["kind"], "single", "{}", events[0]);

    // event.get answers an occurrence with its series from the cache.
    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-series-1"}),
    )
    .await;
    assert_eq!(got["series"]["master_id"], "ev-series");
    assert_eq!(got["series"]["summary"], "Weekly sync");
    let rule = &got["series"]["recurrence"];
    assert_eq!(rule["frequency"], "weekly");
    assert_eq!(rule["count"], 2);
    let single = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-meeting"}),
    )
    .await;
    assert!(single.get("series").is_none(), "{single}");
}

/// An account without a backend views exactly its agenda: the one invitation
/// it holds, at its own instant, addressed `imip:<row_id>`.
#[tokio::test]
async fn a_none_accounts_view_is_its_imip_agenda() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = row_of(slice.root(), IMIP);
    let events = view(&mut conn, json!({"accounts": [IMIP]})).await;
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event["id"], format!("imip:{row_id}"));
    assert_eq!(event["calendar"], "imip");
    assert_eq!(event["uid"], LOCAL_UID);
    assert_eq!(event["summary"], "LOC Day planning");
    assert_eq!(event["location"], "Room 4.12");
    assert_eq!(
        event["time"]["start"],
        at(4, 12).format("%Y-%m-%dT%H:%M:%SZ").to_string()
    );
    assert_eq!(event["own_response"], "needs-action");
    assert_eq!(event["source"]["row_id"], row_id);
    assert_eq!(event["source"]["subject"], "Invitation: LOC Day planning");
    assert_eq!(
        event["source"]["selector"],
        "mp://perso/inbox/loc-day@example.com"
    );

    let id = events[0]["id"].as_str().unwrap();
    let got = call(
        &mut conn,
        "event.get",
        json!({"account": IMIP, "event": id}),
    )
    .await;
    assert_eq!(got["event"]["uid"], LOCAL_UID);
}

/// Every answer reaches the fake with its comment and its send choice, the
/// operation settles with what it did, and the immediate sync leaves the
/// event readable.
#[tokio::test]
async fn event_respond_records_every_answer_on_the_fake() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let mut expected = Vec::new();
    for (word, response) in [
        ("accept", Response::Accept),
        ("tentative", Response::Tentative),
        ("decline", Response::Decline),
    ] {
        for comment in [None, Some("running late")] {
            for send_response in [true, false] {
                let mut params = json!({
                    "account": SERVER,
                    "event": "ev-meeting",
                    "response": word,
                    "send_response": send_response,
                });
                if let Some(comment) = comment {
                    params["comment"] = json!(comment);
                }
                let status = run_operation(&mut conn, "event.respond", params).await;
                let result = succeeded(&status);
                assert_eq!(result["via"], "server", "{result}");
                assert_eq!(result["response"], word);
                assert_eq!(result["send_response"], send_response);
                assert_eq!(result["delivered"], send_response);
                expected.push(RecordedResponse {
                    event: "ev-meeting".into(),
                    response,
                    comment: comment.map(str::to_string),
                    send_response,
                });
            }
        }
    }
    // send_response defaults to true, and a blank comment is none.
    let status = run_operation(
        &mut conn,
        "event.respond",
        json!({"account": SERVER, "event": "ev-meeting", "response": "accept", "comment": "  "}),
    )
    .await;
    succeeded(&status);
    expected.push(RecordedResponse {
        event: "ev-meeting".into(),
        response: Response::Accept,
        comment: None,
        send_response: true,
    });
    assert_eq!(slice.answers(), expected);

    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-meeting"}),
    )
    .await;
    assert_eq!(got["event"]["summary"], "Project review");
}

/// A sign-in without the calendar scope is refused with the re-login prompt
/// before the server is called, and a rebuild's resync reports the same
/// prompt.
#[tokio::test]
async fn event_respond_without_the_scope_is_the_relogin_prompt() {
    let slice = Slice::start_with(true, &[]);
    let mut conn = slice.connect().await;
    let refused = call_err(
        &mut conn,
        "event.respond",
        json!({"account": SERVER, "event": "ev-meeting", "response": "accept"}),
    )
    .await;
    assert_eq!(refused.code, -32603);
    assert_eq!(
        refused.message,
        format!("calendar needs a re-login (mp config oauth2-login -A {SERVER})")
    );
    assert_eq!(
        refused.data.unwrap()["missing_scopes"],
        json!(["Calendars.ReadWrite"])
    );
    assert!(slice.answers().is_empty());

    // The fold still settles the rebuild; the resync reports the prompt.
    let server = rebuild(&mut conn, SERVER).await;
    assert_eq!(
        server["error"]["message"],
        format!("calendar needs a re-login (mp config oauth2-login -A {SERVER})"),
        "{server}"
    );
}

/// An invitation mail's event is answered with an iMIP reply, which cannot
/// leave the organizer uninformed.
#[tokio::test]
async fn event_respond_on_an_invitation_mail_sends_the_imip_reply() {
    // The ledger lives beside the slice, whose root exists only once it is
    // built.
    let ledger = tempfile::tempdir().unwrap();
    let log: PathBuf = ledger.path().join("transport.log");
    let slice = Slice::start_with(
        false,
        &[(
            send_fixture::FAKE_TRANSPORT_ENV,
            send_fixture::fake_transport(&log),
        )],
    );
    let mut conn = slice.connect().await;
    let event = format!("imip:{}", row_of(slice.root(), IMIP));

    let silent = call_err(
        &mut conn,
        "event.respond",
        json!({"account": IMIP, "event": event, "response": "accept", "send_response": false}),
    )
    .await;
    assert_eq!(silent.code, -32602);
    assert!(
        silent.message.contains("send_response: false"),
        "{silent:?}"
    );

    let no_server = call_err(
        &mut conn,
        "event.respond",
        json!({"account": IMIP, "event": "ev-meeting", "response": "accept"}),
    )
    .await;
    assert_eq!(no_server.code, -32602);

    let status = run_operation(
        &mut conn,
        "event.respond",
        json!({"account": IMIP, "event": event, "response": "tentative", "comment": "maybe"}),
    )
    .await;
    let result = succeeded(&status);
    assert_eq!(result["via"], "imip", "{result}");
    assert_eq!(result["delivered"], true, "{result}");
    let submitted = send_fixture::transport_events(&log)
        .into_iter()
        .any(|event| {
            matches!(event, send_fixture::TransportEvent::Submit { address, accepted: true, .. }
                if address == "chair@tum.de")
        });
    assert!(submitted, "the reply reached the organizer");
    assert!(slice.answers().is_empty(), "no server call for a mail");
}

/// A tick syncs the calendar after the mail pass, whatever the mail pass did,
/// and tells the clients.
#[tokio::test]
async fn a_tick_syncs_the_calendar_and_announces_it() {
    let slice = Slice::start();
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({})).await;
    let mut conn = slice.connect().await;

    let listed = call(&mut conn, "calendar.list", json!({"account": SERVER})).await;
    assert_eq!(listed["calendars"].as_array().unwrap().len(), 1, "{listed}");
    run_operation(&mut conn, "sync.quick", json!({"account": SERVER})).await;

    let start = Instant::now();
    let mut announced = false;
    while start.elapsed() < SETTLE_DEADLINE {
        let Some(notification) = within("state.event", watcher.next_notification()).await else {
            break;
        };
        if notification.params["kind"] == "calendar.changed" {
            assert_eq!(notification.params["payload"]["account"], SERVER);
            announced = true;
            break;
        }
    }
    assert!(announced, "a client was told the calendar changed");
    let listed = call(&mut conn, "calendar.list", json!({"account": SERVER})).await;
    assert_eq!(listed["calendars"].as_array().unwrap().len(), 3, "{listed}");
}

/// `message.invite` names the server event of an invitation mail on an
/// account with a backend, and adds nothing on one without.
#[tokio::test]
async fn message_invite_names_the_server_event() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row = row_of(slice.root(), SERVER);
    let answer = call(
        &mut conn,
        "message.invite",
        json!({"account": SERVER, "row_id": row}),
    )
    .await;
    assert_eq!(answer["event_id"], "ev-meeting", "{answer}");
    // Remembered: the second answer is the same.
    let again = call(
        &mut conn,
        "message.invite",
        json!({"account": SERVER, "row_id": row}),
    )
    .await;
    assert_eq!(again["event_id"], "ev-meeting");

    let local = call(
        &mut conn,
        "message.invite",
        json!({"account": IMIP, "row_id": row_of(slice.root(), IMIP)}),
    )
    .await;
    assert!(local.get("event_id").is_none(), "{local}");
    assert_eq!(local["event"]["uid"], LOCAL_UID);
}

/// The zone a view is asked in places the all-day events: the holiday ends
/// at midnight seven days out, which in Los Angeles is seven or eight hours
/// into that day in UTC.
#[tokio::test]
async fn an_all_day_event_is_placed_in_the_viewers_zone() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let mut seen = Vec::new();
    for zone in ["UTC", "America/Los_Angeles"] {
        let answer = call(
            &mut conn,
            "calendar.view",
            json!({
                "start": day(7).to_rfc3339(),
                "end": (day(7) + Days::hours(6)).to_rfc3339(),
                "zone": zone,
                "accounts": [SERVER],
            }),
        )
        .await;
        assert_eq!(answer["zone"], zone);
        seen.push(ids(answer["events"].as_array().unwrap()));
    }
    assert_eq!(seen, [Vec::new(), vec![format!("{SERVER}/ev-holiday")]]);
}

// ---------------------------------------------------------------------------
// event.create (PERSO-255)
// ---------------------------------------------------------------------------

/// The writes the fake recorded, in order.
fn writes(slice: &Slice) -> Vec<RecordedWrite> {
    match std::fs::read_to_string(write_log(&slice.root().join("calendar.jsonl"))) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// A Teams meeting with one attendee and a room, ten days out at 14:00 in
/// Berlin.
fn meeting_input() -> Value {
    json!({
        "summary": "Planning",
        "start": format!("{}T14:00", day(10).date_naive()),
        "duration": "1h",
        "zone": "Europe/Berlin",
        "location": "Room 1",
        "attendees": [
            {"email": "ada@example.com", "name": "Ada Lovelace"},
            {"email": "room-1@example.com", "role": "resource"},
        ],
        "online_meeting": true,
        "reminder": 10,
    })
}

/// A create answers its transaction id at once, reaches the fake with the
/// event resolved, settles with the server's event, which is in the cache
/// before the operation settles and stays there across a resync; a retry
/// with the same transaction id settles with the same event and calls the
/// server no more.
#[tokio::test]
async fn event_create_writes_through_and_a_retry_creates_nothing() {
    let slice = Slice::start();
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({})).await;
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;

    let started = call(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": meeting_input()}),
    )
    .await;
    let transaction_id = started["transaction_id"].as_str().unwrap().to_string();
    assert!(transaction_id.starts_with("mp-"), "{started}");
    let status = wait_for(&mut conn, "event.create", &started).await;
    let result = succeeded(&status).clone();
    assert_eq!(result["account"], SERVER);
    assert_eq!(result["transaction_id"], transaction_id.as_str());
    assert_eq!(
        result["invited"],
        json!(["ada@example.com", "room-1@example.com"])
    );
    let event = &result["event"];
    let id = event["id"].as_str().unwrap().to_string();
    assert_eq!(event["calendar"], "cal-main");
    assert_eq!(event["summary"], "Planning");
    assert_eq!(event["is_organizer"], true);
    assert_eq!(event["is_online_meeting"], true);
    assert_eq!(event["reminder"], 10);
    assert_eq!(event["time"]["zone"], "Europe/Berlin");
    assert_eq!(event["attendees"][1]["role"], "resource");

    let created = writes(&slice);
    assert!(
        matches!(created.as_slice(), [RecordedWrite::Create { transaction_id: t, .. }] if *t == transaction_id),
        "{created:?}"
    );
    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": id}),
    )
    .await;
    assert_eq!(got["event"]["summary"], "Planning");

    // A resync reads the server again, which still holds the event.
    rebuild(&mut conn, SERVER).await;
    let events = view(&mut conn, json!({"accounts": [SERVER]})).await;
    assert!(
        ids(&events).contains(&format!("{SERVER}/{id}")),
        "{events:?}"
    );

    // A retry settles with the cached event and calls the server no more.
    let before = writes(&slice).len();
    let retry = run_operation(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": meeting_input(), "transaction_id": transaction_id}),
    )
    .await;
    assert_eq!(succeeded(&retry)["event"]["id"], id.as_str());
    assert_eq!(writes(&slice).len(), before);

    let start = Instant::now();
    let mut announced = false;
    while start.elapsed() < SETTLE_DEADLINE {
        let Some(notification) = within("state.event", watcher.next_notification()).await else {
            break;
        };
        if notification.params["kind"] == "calendar.changed" {
            announced = true;
            break;
        }
    }
    assert!(announced, "a client was told the calendar changed");
}

/// A retry whose fields changed since the first try made the event (its
/// settle lost, the event synced into the cache) patches that event with
/// the retry's fields instead of settling with the old ones: the summary
/// changes, and the location and the room the retry dropped are cleared.
#[tokio::test]
async fn event_create_retried_with_changes_patches_the_event_it_made() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let first = run_operation(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": meeting_input(), "transaction_id": "tx-edited"}),
    )
    .await;
    let id = succeeded(&first)["event"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut edited = meeting_input();
    edited["summary"] = json!("Planning, moved");
    edited.as_object_mut().unwrap().remove("location");
    edited["attendees"] = json!([{"email": "ada@example.com", "name": "Ada Lovelace"}]);
    let retry = run_operation(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": edited, "transaction_id": "tx-edited"}),
    )
    .await;
    let event = &succeeded(&retry)["event"];
    assert_eq!(event["id"], id.as_str());
    assert_eq!(event["summary"], "Planning, moved");
    assert!(event.get("location").is_none(), "{event}");
    assert_eq!(event["attendees"].as_array().unwrap().len(), 1, "{event}");

    let recorded = writes(&slice);
    let [RecordedWrite::Create { .. }, RecordedWrite::Update { event, patch, .. }] =
        recorded.as_slice()
    else {
        panic!("a create then a patch: {recorded:?}");
    };
    assert_eq!(event, &id);
    assert_eq!(patch.summary.as_deref(), Some("Planning, moved"));
    assert_eq!(patch.location, Some(None));

    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": id}),
    )
    .await;
    assert_eq!(got["event"]["summary"], "Planning, moved");
}

/// An all-day create of two days is the model's all-day range, the end the
/// day after the last, and lands in the calendar it names.
#[tokio::test]
async fn event_create_takes_an_all_day_range_and_a_calendar() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let first = day(12).date_naive();
    let last = day(13).date_naive();
    let status = run_operation(
        &mut conn,
        "event.create",
        json!({
            "account": SERVER,
            "calendar": "cal-second",
            "event": {"summary": "Away", "start": first.to_string(), "end": last.to_string(),
                      "zone": "Europe/Berlin", "show_as": "oof", "reminder": null},
        }),
    )
    .await;
    let event = &succeeded(&status)["event"];
    assert_eq!(event["calendar"], "cal-second");
    assert_eq!(
        event["time"],
        json!({"kind": "all-day", "start": first.to_string(), "end": day(14).date_naive().to_string()})
    );
    assert_eq!(event["show_as"], "oof");
    assert!(event.get("reminder").is_none(), "{event}");
    let recorded = writes(&slice);
    let RecordedWrite::Create { input, .. } = &recorded[0] else {
        panic!("{recorded:?}");
    };
    assert_eq!(input.zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(input.reminder, Some(None));
}

/// Every refusal comes before an operation: an account without a backend,
/// an event typed wrong, a calendar the account does not list, and a Teams
/// meeting on a calendar that offers none; nothing reaches the server.
#[tokio::test]
async fn event_create_refuses_before_an_operation_starts() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let refusal = |account: &str, event: Value, calendar: Option<&str>| {
        let mut params = json!({"account": account, "event": event});
        if let Some(calendar) = calendar {
            params["calendar"] = calendar.into();
        }
        params
    };
    let no_backend = call_err(
        &mut conn,
        "event.create",
        refusal(IMIP, meeting_input(), None),
    )
    .await;
    assert_eq!(no_backend.code, -32602);
    assert!(
        no_backend.message.contains("no calendar backend"),
        "{}",
        no_backend.message
    );

    let mut no_summary = meeting_input();
    no_summary["summary"] = " ".into();
    let bad = call_err(&mut conn, "event.create", refusal(SERVER, no_summary, None)).await;
    assert_eq!(
        (bad.code, bad.message.as_str()),
        (-32602, "An event needs a summary")
    );

    let mut no_end = meeting_input();
    no_end.as_object_mut().unwrap().remove("duration");
    let bad = call_err(&mut conn, "event.create", refusal(SERVER, no_end, None)).await;
    assert_eq!(bad.code, -32602, "{}", bad.message);

    let unknown = call_err(
        &mut conn,
        "event.create",
        refusal(SERVER, meeting_input(), Some("cal-nope")),
    )
    .await;
    assert!(
        unknown.message.contains("has no calendar cal-nope"),
        "{}",
        unknown.message
    );

    let no_teams = call_err(
        &mut conn,
        "event.create",
        refusal(SERVER, meeting_input(), Some("cal-second")),
    )
    .await;
    assert_eq!(
        (no_teams.code, no_teams.message.as_str()),
        (
            -32602,
            "The calendar Holidays of evoqs offers no Teams meetings (the mailbox has no Teams \
             licence)"
        )
    );
    assert!(writes(&slice).is_empty());
}

/// A sign-in without the calendar scope is the re-login prompt, before an
/// operation starts.
#[tokio::test]
async fn event_create_without_the_scope_is_the_relogin_prompt() {
    let slice = Slice::start_with(true, &[]);
    let mut conn = slice.connect().await;
    let refused = call_err(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": meeting_input()}),
    )
    .await;
    assert_eq!(refused.code, -32603);
    assert_eq!(
        refused.message,
        format!("calendar needs a re-login (mp config oauth2-login -A {SERVER})")
    );
    assert!(writes(&slice).is_empty());
}

// ---------------------------------------------------------------------------
// event.update, event.cancel and event.delete (PERSO-255)
// ---------------------------------------------------------------------------

/// The events the change rows work on, beside the fixture's meeting (where
/// the user is an attendee) and holiday: a Teams meeting the user organizes
/// with one attendee, an event of the user's own with nobody invited, the
/// user's copy of a meeting its organizer cancelled, and the two occurrences
/// of the weekly series.
fn change_events() -> Vec<mailypoppins::calendar_model::Event> {
    use mailypoppins::calendar_model::{
        Attendee, AttendeeRole, EventTime, OnlineMeetingProvider, OwnResponse, Person,
        ResponseStatus,
    };
    use support::calendar_fixture::event;
    let berlin = |days, hour| EventTime::timed(at(days, hour), at(days, hour + 1), "Europe/Berlin");
    let mut own = event(
        "ev-own",
        "cal-main",
        "own@example.com",
        "Budget",
        berlin(8, 9).unwrap(),
    );
    own.is_organizer = true;
    own.own_response = OwnResponse::Organizer;
    own.location = Some("Room 1".into());
    own.reminder = Some(15);
    own.is_online_meeting = true;
    own.online_meeting_provider = Some(OnlineMeetingProvider::TeamsForBusiness);
    own.attendees = vec![Attendee {
        name: Some("Ada Lovelace".into()),
        email: "ada@example.com".into(),
        role: AttendeeRole::Required,
        status: ResponseStatus::Accepted,
        proposed_time: None,
    }];
    let mut solo = event(
        "ev-solo",
        "cal-second",
        "solo@example.com",
        "Dentist",
        berlin(9, 7).unwrap(),
    );
    solo.is_organizer = true;
    solo.own_response = OwnResponse::Organizer;
    let mut called_off = event(
        "ev-called-off",
        "cal-main",
        "called-off@example.com",
        "Canceled: Offsite",
        berlin(11, 9).unwrap(),
    );
    called_off.cancelled = true;
    called_off.organizer = Some(Person {
        name: None,
        email: "chair@tum.de".into(),
    });
    called_off.own_response = OwnResponse::Accepted;
    [vec![own, solo, called_off], series_occurrences()].concat()
}

fn change_slice() -> Slice {
    Slice::start_seeded(false, &[], |_| (), change_events())
}

/// An update by the organizer answers an operation id, reaches the fake with
/// the fields it names only (the new start keeping the meeting's hour, the
/// location cleared), settles with the server's event, which the cache holds
/// before the operation settles, and is announced.
#[tokio::test]
async fn event_update_patches_the_fields_it_names_and_writes_through() {
    let slice = change_slice();
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({})).await;
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;

    let start = format!("{}T15:00", day(8).date_naive());
    let status = run_operation(
        &mut conn,
        "event.update",
        json!({"account": SERVER, "event": "ev-own",
               "fields": {"start": start, "location": null, "reminder": 5}}),
    )
    .await;
    let result = succeeded(&status);
    assert_eq!(result["account"], SERVER);
    let event = &result["event"];
    assert_eq!(event["id"], "ev-own");
    assert_eq!(event["summary"], "Budget");
    assert!(event.get("location").is_none(), "{event}");
    assert_eq!(event["reminder"], 5);
    let wall = day(8).date_naive().and_hms_opt(15, 0, 0).unwrap();
    let expected = chrono::TimeZone::from_local_datetime(&chrono_tz::Europe::Berlin, &wall)
        .unwrap()
        .to_utc();
    let instant = |key: &str| {
        chrono::DateTime::parse_from_rfc3339(event["time"][key].as_str().unwrap())
            .unwrap()
            .to_utc()
    };
    assert_eq!(event["time"]["zone"], "Europe/Berlin");
    assert_eq!(instant("start"), expected);
    assert_eq!(instant("end"), expected + Days::hours(1));

    let recorded = writes(&slice);
    let [RecordedWrite::Update {
        calendar,
        event: id,
        patch,
    }] = recorded.as_slice()
    else {
        panic!("one update: {recorded:?}");
    };
    assert_eq!((calendar.as_str(), id.as_str()), ("cal-main", "ev-own"));
    assert!(patch.time.is_some());
    assert_eq!(patch.zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(patch.location, Some(None));
    assert_eq!(patch.reminder, Some(Some(5)));
    assert_eq!(patch.summary, None);
    assert_eq!(patch.body, None);
    assert_eq!(patch.attendees, None);

    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-own"}),
    )
    .await;
    assert_eq!(got["event"]["reminder"], 5);
    assert_eq!(got["event"]["time"], event["time"]);

    let start = Instant::now();
    let mut announced = false;
    while start.elapsed() < SETTLE_DEADLINE {
        let Some(notification) = within("state.event", watcher.next_notification()).await else {
            break;
        };
        if notification.params["kind"] == "calendar.changed" {
            announced = true;
            break;
        }
    }
    assert!(announced, "a client was told the calendar changed");
}

/// An attendee changes their own copy's reminder and show-as and nothing
/// else; every other refusal of an update comes before an operation, and
/// nothing refused reaches the server.
#[tokio::test]
async fn event_update_refuses_before_an_operation_starts() {
    let slice = change_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let update = |account: &str, event: &str, fields: Value| json!({"account": account, "event": event, "fields": fields});
    let refused = |error: RpcError| {
        assert_eq!(error.code, -32602, "{}", error.message);
        error.message
    };

    let attendee = call_err(
        &mut conn,
        "event.update",
        update(SERVER, "ev-meeting", json!({"summary": "Mine now"})),
    )
    .await;
    assert_eq!(
        refused(attendee),
        "Only the organizer can change this event"
    );
    let own_copy = run_operation(
        &mut conn,
        "event.update",
        update(
            SERVER,
            "ev-meeting",
            json!({"reminder": null, "show_as": "free"}),
        ),
    )
    .await;
    assert_eq!(succeeded(&own_copy)["event"]["show_as"], "free");
    let before = writes(&slice).len();
    assert_eq!(before, 1);

    let cases = [
        (update(SERVER, "ev-own", json!({})), "Nothing to change"),
        (
            update(SERVER, "ev-own", json!({"online_meeting": false})),
            "stays a Teams meeting",
        ),
        (
            update(SERVER, "ev-own", json!({"body": "New agenda"})),
            "cannot be changed from mp yet",
        ),
        (
            update(SERVER, "ev-solo", json!({"online_meeting": true})),
            "offers no Teams meetings",
        ),
        (
            update(SERVER, "ev-series-1", json!({"summary": "Renamed"})),
            "is one occurrence of a series: name the scope",
        ),
        (
            update(SERVER, "ev-nope", json!({"summary": "Renamed"})),
            "has no event ev-nope",
        ),
        (
            update(SERVER, "imip:1", json!({"summary": "Renamed"})),
            "can only be answered",
        ),
        (
            update(IMIP, "ev-own", json!({"summary": "Renamed"})),
            "has no calendar backend",
        ),
        (
            update(SERVER, "ev-own", json!({"summary": " "})),
            "An event needs a summary",
        ),
    ];
    for (params, words) in cases {
        let message = refused(call_err(&mut conn, "event.update", params.clone()).await);
        assert!(message.contains(words), "{params}: {message}");
    }
    assert_eq!(writes(&slice).len(), before);
}

/// A cancel by the organizer tells the attendees with the comment, takes
/// the meeting out of the cache before it settles and names who was told;
/// an attendee's cancel and a cancel of an event that invites nobody are
/// refused before an operation starts.
#[tokio::test]
async fn event_cancel_tells_the_attendees_and_removes_the_meeting() {
    let slice = change_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;

    for (event, words) in [
        ("ev-meeting", "Only the organizer can cancel Project review"),
        ("ev-solo", "Dentist invites nobody"),
        (
            "ev-series-2",
            "is one occurrence of a series: name the scope",
        ),
    ] {
        let error = call_err(
            &mut conn,
            "event.cancel",
            json!({"account": SERVER, "event": event}),
        )
        .await;
        assert_eq!(error.code, -32602);
        assert!(error.message.contains(words), "{}", error.message);
    }
    assert!(writes(&slice).is_empty());

    let status = run_operation(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": "ev-own", "comment": "Postponed"}),
    )
    .await;
    assert_eq!(
        succeeded(&status),
        &json!({"account": SERVER, "event": "ev-own", "notified": ["ada@example.com"]})
    );
    assert_eq!(
        writes(&slice),
        [RecordedWrite::Cancel {
            event: "ev-own".into(),
            comment: Some("Postponed".into()),
        }]
    );
    let events = view(&mut conn, json!({"accounts": [SERVER]})).await;
    assert!(
        !ids(&events).contains(&format!("{SERVER}/ev-own")),
        "{events:?}"
    );

    // A retry, as a client whose operation vanished sends it, counts as done
    // with the first call's answer and reaches the server no more; a delete
    // of the cancelled meeting and a cancel of an id the cache never held
    // are still refused.
    let retry = run_operation(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": "ev-own", "comment": "Postponed"}),
    )
    .await;
    assert_eq!(succeeded(&retry), succeeded(&status));
    assert_eq!(writes(&slice).len(), 1);
    for (method, event) in [("event.delete", "ev-own"), ("event.cancel", "ev-nope")] {
        let unknown = call_err(
            &mut conn,
            method,
            json!({"account": SERVER, "event": event}),
        )
        .await;
        assert_eq!(unknown.code, -32602);
        assert!(
            unknown.message.contains(&format!(
                "has no event {event} (cancelled or deleted already"
            )),
            "{method}: {}",
            unknown.message
        );
    }
    assert_eq!(writes(&slice).len(), 1);
}

/// A cancel or a delete of an event the cache still holds but the server
/// lost already (the daemon died after the server answered and before the
/// cache heard of it) reaches the server, whose "gone" counts as done, and
/// leaves the cache.
#[tokio::test]
async fn a_cancel_or_delete_the_server_lost_already_counts_as_done() {
    use mailypoppins::calendar_model::{
        Attendee, AttendeeRole, EventTime, OwnResponse, ResponseStatus,
    };
    let slice = change_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let time = || EventTime::timed(at(12, 9), at(12, 10), "Europe/Berlin").unwrap();
    let mut meeting = support::calendar_fixture::event(
        "ev-lost-meeting",
        "cal-main",
        "lost-meeting@example.com",
        "Lost meeting",
        time(),
    );
    meeting.is_organizer = true;
    meeting.own_response = OwnResponse::Organizer;
    meeting.attendees = vec![Attendee {
        name: None,
        email: "ada@example.com".into(),
        role: AttendeeRole::Required,
        status: ResponseStatus::Accepted,
        proposed_time: None,
    }];
    let mut solo = support::calendar_fixture::event(
        "ev-lost-solo",
        "cal-main",
        "lost-solo@example.com",
        "Lost dentist",
        time(),
    );
    solo.is_organizer = true;
    solo.own_response = OwnResponse::Organizer;
    let store = read_fixture::store(slice.root(), SERVER);
    for event in [&meeting, &solo] {
        mailypoppins::store::calendar::upsert_event(&store, SERVER, event).unwrap();
    }

    let cancelled = run_operation(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": "ev-lost-meeting"}),
    )
    .await;
    assert_eq!(
        succeeded(&cancelled),
        &json!({"account": SERVER, "event": "ev-lost-meeting", "notified": ["ada@example.com"]})
    );
    let deleted = run_operation(
        &mut conn,
        "event.delete",
        json!({"account": SERVER, "event": "ev-lost-solo"}),
    )
    .await;
    assert_eq!(
        succeeded(&deleted),
        &json!({"account": SERVER, "event": "ev-lost-solo"})
    );
    for id in ["ev-lost-meeting", "ev-lost-solo"] {
        assert_eq!(
            mailypoppins::store::calendar::get_event(&store, SERVER, id).unwrap(),
            None,
            "{id}"
        );
    }
}

/// A delete tells nobody: an event without attendees and the user's copy of
/// a cancelled meeting go, from the server and the cache; a live meeting is
/// refused, the organizer's with "cancel it" and an attendee's with
/// "decline it".
#[tokio::test]
async fn event_delete_removes_an_event_that_tells_nobody() {
    let slice = change_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;

    for (event, words) in [
        ("ev-own", "Budget is a meeting with attendees; cancel it"),
        (
            "ev-meeting",
            "Project review is a meeting you were invited to; decline it",
        ),
    ] {
        let error = call_err(
            &mut conn,
            "event.delete",
            json!({"account": SERVER, "event": event}),
        )
        .await;
        assert_eq!(error.code, -32602);
        assert!(error.message.contains(words), "{}", error.message);
    }
    assert!(writes(&slice).is_empty());

    for event in ["ev-solo", "ev-called-off"] {
        let status = run_operation(
            &mut conn,
            "event.delete",
            json!({"account": SERVER, "event": event}),
        )
        .await;
        assert_eq!(
            succeeded(&status),
            &json!({"account": SERVER, "event": event})
        );
    }
    assert_eq!(
        writes(&slice),
        [
            RecordedWrite::Delete {
                event: "ev-solo".into()
            },
            RecordedWrite::Delete {
                event: "ev-called-off".into()
            },
        ]
    );
    let events = view(
        &mut conn,
        json!({"accounts": [SERVER], "include_hidden": true}),
    )
    .await;
    let listed = ids(&events);
    assert!(!listed.contains(&format!("{SERVER}/ev-solo")), "{listed:?}");
    assert!(
        !listed.contains(&format!("{SERVER}/ev-called-off")),
        "{listed:?}"
    );

    // A retry counts as done and reaches the server no more; a cancel of the
    // deleted event is still refused.
    let retry = run_operation(
        &mut conn,
        "event.delete",
        json!({"account": SERVER, "event": "ev-solo"}),
    )
    .await;
    assert_eq!(
        succeeded(&retry),
        &json!({"account": SERVER, "event": "ev-solo"})
    );
    assert_eq!(writes(&slice).len(), 2);
    let unknown = call_err(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": "ev-solo"}),
    )
    .await;
    assert!(
        unknown.message.contains("has no event ev-solo"),
        "{}",
        unknown.message
    );
}

/// An update, a cancel or a delete of an event on a calendar that takes no
/// writes is refused before an operation starts, and nothing reaches the
/// server.
#[tokio::test]
async fn event_changes_on_a_read_only_calendar_are_refused() {
    let slice = change_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let store = read_fixture::store(slice.root(), SERVER);
    let calendars: Vec<_> = mailypoppins::store::calendar::list_calendars(&store, SERVER)
        .unwrap()
        .into_iter()
        .map(|mut calendar| {
            calendar.can_edit = false;
            calendar
        })
        .collect();
    mailypoppins::store::calendar::upsert_calendars(&store, SERVER, &calendars).unwrap();

    for (method, params, name) in [
        (
            "event.update",
            json!({"account": SERVER, "event": "ev-own", "fields": {"reminder": 5}}),
            "Calendar",
        ),
        (
            "event.cancel",
            json!({"account": SERVER, "event": "ev-own", "comment": "Postponed"}),
            "Calendar",
        ),
        (
            "event.delete",
            json!({"account": SERVER, "event": "ev-solo"}),
            "Holidays",
        ),
    ] {
        let refused = call_err(&mut conn, method, params).await;
        assert_eq!(
            (refused.code, refused.message.as_str()),
            (
                -32602,
                format!("The calendar {name} of {SERVER} is read-only").as_str()
            ),
            "{method}"
        );
    }
    assert!(writes(&slice).is_empty());
    for id in ["ev-own", "ev-solo"] {
        assert!(
            mailypoppins::store::calendar::get_event(&store, SERVER, id)
                .unwrap()
                .is_some(),
            "{id} stays in the cache"
        );
    }
}

/// An update the server refuses fails the operation with the server's
/// error, and the cache keeps the event as it was.
#[tokio::test]
async fn an_update_the_server_refuses_leaves_the_cache_as_it_was() {
    use mailypoppins::calendar_model::{EventTime, OwnResponse};
    let slice = change_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    // An event the cache holds and the server does not, which the fake
    // refuses to update as Graph answers 404.
    let mut lost = support::calendar_fixture::event(
        "ev-lost",
        "cal-main",
        "lost@example.com",
        "Lost review",
        EventTime::timed(at(12, 9), at(12, 10), "Europe/Berlin").unwrap(),
    );
    lost.is_organizer = true;
    lost.own_response = OwnResponse::Organizer;
    lost.reminder = Some(15);
    let store = read_fixture::store(slice.root(), SERVER);
    mailypoppins::store::calendar::upsert_event(&store, SERVER, &lost).unwrap();
    let before = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-lost"}),
    )
    .await;

    let status = run_operation(
        &mut conn,
        "event.update",
        json!({"account": SERVER, "event": "ev-lost",
               "fields": {"summary": "Found review", "reminder": 5}}),
    )
    .await;
    assert_eq!(status["state"], "failed", "{status}");
    let error = status["error"]["message"].as_str().unwrap_or_default();
    assert!(error.contains("no event with id ev-lost"), "{status}");
    assert!(writes(&slice).is_empty());

    let after = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-lost"}),
    )
    .await;
    assert_eq!(after, before);
    assert_eq!(after["event"]["summary"], "Lost review");
    assert_eq!(after["event"]["reminder"], 15);
}

/// A sign-in without the calendar scope is the re-login prompt for every
/// change, before an operation starts.
#[tokio::test]
async fn event_changes_without_the_scope_are_the_relogin_prompt() {
    let slice = Slice::start_seeded(true, &[], |_| (), change_events());
    let mut conn = slice.connect().await;
    for (method, params) in [
        (
            "event.update",
            json!({"account": SERVER, "event": "ev-own", "fields": {"reminder": 5}}),
        ),
        (
            "event.cancel",
            json!({"account": SERVER, "event": "ev-own"}),
        ),
        (
            "event.delete",
            json!({"account": SERVER, "event": "ev-solo"}),
        ),
    ] {
        let refused = call_err(&mut conn, method, params).await;
        assert_eq!(refused.code, -32603, "{method}");
        assert_eq!(
            refused.message,
            format!("calendar needs a re-login (mp config oauth2-login -A {SERVER})")
        );
    }
    assert!(writes(&slice).is_empty());
}

// ---------------------------------------------------------------------------
// Recurrence and scopes (PERSO-255, slice 5)
// ---------------------------------------------------------------------------

/// An organizer's series of `count` weekly occurrences from `first` days
/// out at 08:00 UTC, inviting `attendees`, with its occurrences named as the
/// fake names the ones it expands: the master's id and their date.
fn own_series(
    id: &str,
    first: i64,
    count: u32,
    attendees: &[&str],
) -> (
    mailypoppins::calendar_backend::SeriesInfo,
    Vec<mailypoppins::calendar_model::Event>,
) {
    use mailypoppins::calendar_model::{
        Attendee, AttendeeRole, EventKind, EventTime, Frequency, OwnResponse, Recurrence,
        ResponseStatus,
    };
    let time = |days: i64| EventTime::timed(at(days, 8), at(days, 9), "Europe/Berlin").unwrap();
    let mut master = support::calendar_fixture::event(
        id,
        "cal-main",
        &format!("{id}@example.com"),
        "Standup",
        time(first),
    );
    master.kind = EventKind::SeriesMaster;
    master.is_organizer = true;
    master.own_response = OwnResponse::Organizer;
    master.attendees = attendees
        .iter()
        .map(|email| Attendee {
            name: None,
            email: email.to_string(),
            role: AttendeeRole::Required,
            status: ResponseStatus::Accepted,
            proposed_time: None,
        })
        .collect();
    let rule = Recurrence {
        frequency: Frequency::Weekly,
        interval: 1,
        weekdays: vec![mailypoppins::calendar_model::Weekday::of_date(
            day(first).date_naive(),
        )],
        day_of_month: None,
        week_index: None,
        month: None,
        first_day_of_week: None,
        start: Some(day(first).date_naive()),
        until: None,
        count: Some(count),
    };
    master.recurrence = Some(rule.describe());
    let occurrences = (0..i64::from(count))
        .map(|n| {
            let days = first + 7 * n;
            let mut occurrence = master.clone();
            occurrence.id = occurrence_id(id, days);
            occurrence.kind = EventKind::Occurrence;
            occurrence.series_master_id = Some(id.to_string());
            occurrence.time = time(days);
            occurrence.recurrence_id = Some(at(days, 8).to_rfc3339());
            occurrence
        })
        .collect();
    let series = mailypoppins::calendar_backend::SeriesInfo {
        master_id: id.into(),
        uid: format!("{id}@example.com"),
        recurrence: Some(rule),
        master: Some(Box::new(master)),
        zone: Some("Europe/Berlin".into()),
    };
    (series, occurrences)
}

/// The id of the occurrence of series `master` `days` out.
fn occurrence_id(master: &str, days: i64) -> String {
    format!("{master}-{}", day(days).date_naive().format("%Y%m%d"))
}

/// The change rows' fake plus two series the user organizes: a standup of
/// three weekly occurrences from ten days out inviting Ada, and a focus
/// block of two from twelve days out inviting nobody.
fn series_slice() -> Slice {
    let (standup, standups) = own_series("ev-standup", 10, 3, &["ada@example.com"]);
    let (focus, focuses) = own_series("ev-focus", 12, 2, &[]);
    Slice::start_spec(|log| {
        let events = [change_events(), standups, focuses].concat();
        let mut spec: Value = serde_json::from_str(&fake_spec_with(log, false, events)).unwrap();
        spec["series"].as_array_mut().unwrap().extend([
            serde_json::to_value(standup).unwrap(),
            serde_json::to_value(focus).unwrap(),
        ]);
        spec.to_string()
    })
}

/// The summary of each cached occurrence of `master`, by id.
async fn occurrences_of(conn: &mut Connection, master: &str) -> Vec<(String, String)> {
    let events = view(conn, json!({"accounts": [SERVER]})).await;
    let mut listed: Vec<(String, String)> = events
        .iter()
        .filter(|e| e["series_master_id"] == master)
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_string(),
                e["summary"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    listed.sort();
    listed
}

/// A create with a rule reaches the fake with the rule read against the
/// start's day, settles with the series master, whose occurrences the sync
/// after it brings into the view; `event.get` answers the master with its
/// rule, and a retry of the transaction settles with the same series.
#[tokio::test]
async fn a_recurring_create_makes_a_series_the_sync_fills() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let mut input = meeting_input();
    input["recurrence"] = json!({"frequency": "weekly", "interval": 1, "count": 3});
    let status = run_operation(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": input, "transaction_id": "tx-series"}),
    )
    .await;
    let event = succeeded(&status)["event"].clone();
    let weekday = mailypoppins::calendar_model::Weekday::of_date(day(10).date_naive());
    assert_eq!(event["kind"], "series-master", "{event}");
    assert_eq!(
        event["recurrence"],
        format!("Weekly on {} (3 times)", weekday.name())
    );
    let id = event["id"].as_str().unwrap().to_string();
    let recorded = writes(&slice);
    let [RecordedWrite::Create { input, .. }] = recorded.as_slice() else {
        panic!("one create: {recorded:?}");
    };
    let rule = input.recurrence.as_ref().unwrap();
    assert_eq!(rule.weekdays, [weekday]);
    assert_eq!(rule.start, Some(day(10).date_naive()));

    let listed = occurrences_of(&mut conn, &id).await;
    assert_eq!(listed.len(), 3, "{listed:?}");
    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": id}),
    )
    .await;
    assert_eq!(got["event"]["kind"], "series-master");
    assert_eq!(got["series"]["master_id"], id.as_str());
    assert_eq!(got["series"]["recurrence"]["count"], 3);

    let retry = run_operation(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": meeting_input_with_rule(), "transaction_id": "tx-series"}),
    )
    .await;
    assert_eq!(succeeded(&retry)["event"]["id"], id.as_str());
    assert_eq!(writes(&slice).len(), 1);

    // A retry with a shorter rule patches the series, and the occurrence it
    // no longer places leaves the view.
    let mut shorter = meeting_input_with_rule();
    shorter["recurrence"]["count"] = 2.into();
    let patched = run_operation(
        &mut conn,
        "event.create",
        json!({"account": SERVER, "event": shorter, "transaction_id": "tx-series"}),
    )
    .await;
    assert_eq!(succeeded(&patched)["event"]["id"], id.as_str());
    assert_eq!(writes(&slice).len(), 2);
    let listed = occurrences_of(&mut conn, &id).await;
    assert_eq!(listed.len(), 2, "{listed:?}");
}

fn meeting_input_with_rule() -> Value {
    let mut input = meeting_input();
    input["recurrence"] = json!({"frequency": "weekly", "interval": 1, "count": 3});
    input
}

/// An occurrence's id needs its scope, and a scope that does not fit the
/// event is refused before an operation starts, as a rule outside the
/// series scope is; nothing refused reaches the server.
#[tokio::test]
async fn a_scope_that_does_not_fit_is_refused() {
    let slice = series_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let first = occurrence_id("ev-standup", 10);
    let rule = json!({"frequency": "daily", "interval": 1});
    let cases = [
        (
            "event.update",
            json!({"event": first, "fields": {"summary": "Renamed"}}),
            "Standup is one occurrence of a series: name the scope, occurrence or series",
        ),
        ("event.cancel", json!({"event": first}), "name the scope"),
        (
            "event.delete",
            json!({"event": occurrence_id("ev-focus", 12)}),
            "name the scope",
        ),
        (
            "event.update",
            json!({"event": "ev-solo", "scope": "occurrence", "fields": {"summary": "x"}}),
            "Dentist is not an occurrence of a series",
        ),
        (
            "event.delete",
            json!({"event": "ev-solo", "scope": "series"}),
            "Dentist is not part of a series",
        ),
        (
            "event.update",
            json!({"event": "ev-standup", "scope": "occurrence", "fields": {"summary": "x"}}),
            "is a whole series",
        ),
        (
            "event.update",
            json!({"event": first, "scope": "occurrence", "fields": {"recurrence": rule}}),
            "An occurrence repeats by its series' rule",
        ),
        (
            "event.update",
            json!({"event": "ev-solo", "fields": {"recurrence": rule}}),
            "Dentist does not repeat",
        ),
        (
            "event.update",
            json!({"event": first, "scope": "following", "fields": {"summary": "x"}}),
            "use one of occurrence, series",
        ),
        (
            "event.update",
            json!({"event": first, "scope": "series",
                   "fields": {"recurrence": {"frequency": "weekly", "interval": 1, "count": 0}}}),
            "A series has 1 to 999 occurrences",
        ),
    ];
    for (method, mut params, words) in cases {
        params["account"] = SERVER.into();
        let error = call_err(&mut conn, method, params.clone()).await;
        assert_eq!(error.code, -32602, "{params}");
        assert!(error.message.contains(words), "{params}: {}", error.message);
    }
    assert!(writes(&slice).is_empty());
}

/// A change to one occurrence patches that occurrence, which becomes an
/// exception; a change to the series patches its master, reaches every
/// occurrence not changed apart and settles with the master; a new rule
/// resets the exception and ends the occurrences it no longer places, which
/// the server's delta never names and the baseline after the change drops.
#[tokio::test]
async fn an_occurrence_and_the_series_change_in_their_scope() {
    let slice = series_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let [first, second, third] = [10, 17, 24].map(|days| occurrence_id("ev-standup", days));

    let moved = run_operation(
        &mut conn,
        "event.update",
        json!({"account": SERVER, "event": second, "scope": "occurrence",
               "fields": {"summary": "Standup, moved"}}),
    )
    .await;
    let result = succeeded(&moved);
    assert_eq!(result["scope"], "occurrence");
    assert_eq!(result["event"]["id"], second.as_str());
    assert_eq!(result["event"]["kind"], "exception");

    let renamed = run_operation(
        &mut conn,
        "event.update",
        json!({"account": SERVER, "event": first, "scope": "series",
               "fields": {"summary": "Daily standup"}}),
    )
    .await;
    let result = succeeded(&renamed);
    assert_eq!(result["scope"], "series");
    assert_eq!(result["event"]["id"], "ev-standup");
    assert_eq!(result["event"]["kind"], "series-master");
    assert_eq!(result["event"]["summary"], "Daily standup");
    assert_eq!(
        occurrences_of(&mut conn, "ev-standup").await,
        [
            (first.clone(), "Daily standup".to_string()),
            (second.clone(), "Standup, moved".to_string()),
            (third.clone(), "Daily standup".to_string()),
        ]
    );

    let reruled = run_operation(
        &mut conn,
        "event.update",
        json!({"account": SERVER, "event": "ev-standup", "scope": "series",
               "fields": {"recurrence": {"frequency": "weekly", "interval": 1, "count": 2}}}),
    )
    .await;
    assert_eq!(succeeded(&reruled)["event"]["recurrence"], {
        let weekday = mailypoppins::calendar_model::Weekday::of_date(day(10).date_naive());
        format!("Weekly on {} (2 times)", weekday.name())
    });
    assert_eq!(
        occurrences_of(&mut conn, "ev-standup").await,
        [
            (first.clone(), "Daily standup".to_string()),
            (second.clone(), "Daily standup".to_string()),
        ]
    );
    let got = call(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": first}),
    )
    .await;
    assert_eq!(got["series"]["recurrence"]["count"], 2);

    let recorded = writes(&slice);
    let updated: Vec<&str> = recorded
        .iter()
        .map(|write| match write {
            RecordedWrite::Update { event, .. } => event.as_str(),
            other => panic!("only updates: {other:?}"),
        })
        .collect();
    assert_eq!(updated, [second.as_str(), "ev-standup", "ev-standup"]);
}

/// A cancel of one occurrence tells the attendees of it and takes it out of
/// the cache, the others staying; a cancel of the series cancels its master
/// and takes every occurrence and the series out; a retry in the same scope
/// counts as done, and one in another scope is refused.
#[tokio::test]
async fn an_occurrence_or_the_series_is_cancelled_in_its_scope() {
    let slice = series_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let [first, second, third] = [10, 17, 24].map(|days| occurrence_id("ev-standup", days));

    let one = run_operation(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": first, "scope": "occurrence", "comment": "Off"}),
    )
    .await;
    assert_eq!(
        succeeded(&one),
        &json!({"account": SERVER, "event": first, "notified": ["ada@example.com"],
                "scope": "occurrence"})
    );
    let listed: Vec<String> = occurrences_of(&mut conn, "ev-standup")
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(listed, [second.clone(), third.clone()]);

    let all = run_operation(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": second, "scope": "series"}),
    )
    .await;
    let settled = json!({"account": SERVER, "event": second, "notified": ["ada@example.com"],
                         "scope": "series"});
    assert_eq!(succeeded(&all), &settled);
    assert!(occurrences_of(&mut conn, "ev-standup").await.is_empty());
    let gone = call_err(
        &mut conn,
        "event.get",
        json!({"account": SERVER, "event": "ev-standup"}),
    )
    .await;
    assert_eq!(gone.code, -32602);
    assert_eq!(
        writes(&slice),
        [
            RecordedWrite::Cancel {
                event: first.clone(),
                comment: Some("Off".into()),
            },
            RecordedWrite::Cancel {
                event: "ev-standup".into(),
                comment: None,
            },
        ]
    );

    let retry = run_operation(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": second, "scope": "series"}),
    )
    .await;
    assert_eq!(succeeded(&retry), &settled);
    let other = call_err(
        &mut conn,
        "event.cancel",
        json!({"account": SERVER, "event": second, "scope": "occurrence"}),
    )
    .await;
    assert!(other.message.contains("has no event"), "{}", other.message);
    assert_eq!(writes(&slice).len(), 2);
}

/// A delete of one occurrence of a series that tells nobody takes it alone,
/// a delete of the series takes them all, and a series with attendees is
/// cancelled rather than deleted.
#[tokio::test]
async fn an_occurrence_or_the_series_is_deleted_in_its_scope() {
    let slice = series_slice();
    let mut conn = slice.connect().await;
    rebuild(&mut conn, SERVER).await;
    let [first, second] = [12, 19].map(|days| occurrence_id("ev-focus", days));

    let refused = call_err(
        &mut conn,
        "event.delete",
        json!({"account": SERVER, "event": occurrence_id("ev-standup", 10), "scope": "series"}),
    )
    .await;
    assert!(refused.message.contains("cancel it"), "{}", refused.message);

    let one = run_operation(
        &mut conn,
        "event.delete",
        json!({"account": SERVER, "event": first, "scope": "occurrence"}),
    )
    .await;
    assert_eq!(
        succeeded(&one),
        &json!({"account": SERVER, "event": first, "scope": "occurrence"})
    );
    let listed: Vec<String> = occurrences_of(&mut conn, "ev-focus")
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(listed, std::slice::from_ref(&second));
    let all = run_operation(
        &mut conn,
        "event.delete",
        json!({"account": SERVER, "event": "ev-focus", "scope": "series"}),
    )
    .await;
    assert_eq!(
        succeeded(&all),
        &json!({"account": SERVER, "event": "ev-focus", "scope": "series"})
    );
    assert!(occurrences_of(&mut conn, "ev-focus").await.is_empty());
    assert_eq!(
        writes(&slice),
        [
            RecordedWrite::Delete { event: first },
            RecordedWrite::Delete {
                event: "ev-focus".into()
            },
        ]
    );
}
