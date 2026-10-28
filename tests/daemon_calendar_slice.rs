//! The server calendar over the daemon (PERSO-200, phase 1).
//!
//! Every row runs a real `mp daemon run` against a sandboxed root with the
//! calendar fake armed through `MAILYPOPPINS_DAEMON_FAKE_CALENDAR`, and talks to
//! it over the socket:
//!
//! - `calendar.list` lists the fake's calendars and each account's virtual
//!   invitation calendar;
//! - `calendar.view` merges the server cache and the invitation mails, keeps
//!   the server copy of an invitation the server also has, hides a hidden
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

use chrono::{DateTime, Duration as Days, NaiveTime, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::{RecordedResponse, FAKE_CALENDAR_ENV};
use mailypoppins::calendar_backend::Response;
use mailypoppins::calendar_model::{
    Attendee, AttendeeRole, Calendar, Event, EventTime, OwnResponse, Person, ResponseStatus,
    Sensitivity, ShowAs,
};
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture;
use support::send_fixture;

const DEADLINE: Duration = Duration::from_secs(20);
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The account with a calendar backend: a password account that names
/// `graph`, which the fake stands in for.
const SERVER: &str = "evoqs";
/// The account without one.
const IMIP: &str = "perso";

/// The `UID` the server's meeting and the invitation mail about it share.
const MEETING_UID: &str = "meeting-1@example.com";
/// The `UID` of the invitation only the account without a backend holds.
const LOCAL_UID: &str = "loc-day@tum.de";
/// The Graph id the server account's invitation row is bound to.
const INVITE_GRAPH_ID: &str = "AAMkAD-invite";

const CONFIG: &str = r#"
[[accounts]]
name = "evoqs"
default_from = "sylvain@example.com"
calendar_backend = "graph"

[accounts.smtp]
port = 465

[accounts.imap]
host = "127.0.0.1"
port = 9
username = "sylvain@example.com"
sync_interval_secs = 0

[[accounts]]
name = "perso"
default_from = "perso@example.com"

[accounts.smtp]
port = 465

[accounts.imap]
port = 993
sync_interval_secs = 0
"#;

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// Midnight UTC `days` from today.
fn day(days: i64) -> DateTime<Utc> {
    (Utc::now().date_naive() + Days::days(days))
        .and_time(NaiveTime::MIN)
        .and_utc()
}

/// `days` from today at `hour` UTC.
fn at(days: i64, hour: i64) -> DateTime<Utc> {
    day(days) + Days::hours(hour)
}

fn ics_time(instant: DateTime<Utc>) -> String {
    instant.format("%Y%m%dT%H%M%SZ").to_string()
}

fn calendar(id: &str, name: &str, is_default: bool) -> Calendar {
    Calendar {
        account: "spec".into(),
        id: id.into(),
        name: name.into(),
        color: Some("#3b82f6".into()),
        is_default,
        can_edit: true,
        visible: true,
    }
}

fn event(id: &str, calendar: &str, uid: &str, summary: &str, time: EventTime) -> Event {
    Event {
        account: "spec".into(),
        calendar: calendar.into(),
        id: id.into(),
        uid: uid.into(),
        recurrence_id: None,
        series_master_id: None,
        summary: summary.into(),
        time,
        location: None,
        body: None,
        organizer: None,
        attendees: Vec::new(),
        own_response: OwnResponse::None,
        recurrence: None,
        reminder: None,
        online_meeting_url: None,
        sensitivity: Sensitivity::Normal,
        show_as: ShowAs::Busy,
        cancelled: false,
        is_organizer: false,
        last_modified: None,
        web_link: None,
    }
}

/// The fake's server: a default and a second calendar, a meeting the server
/// account was also invited to by mail, and an all-day holiday.
fn fake_spec(log: &Path, missing_scope: bool) -> String {
    let mut meeting = event(
        "ev-meeting",
        "cal-main",
        MEETING_UID,
        "Project review",
        EventTime::timed(at(3, 9), at(3, 10), "Europe/Berlin").unwrap(),
    );
    meeting.organizer = Some(Person {
        name: Some("Ada Lovelace".into()),
        email: "ada@example.com".into(),
    });
    meeting.attendees = vec![Attendee {
        name: None,
        email: "sylvain@example.com".into(),
        role: AttendeeRole::Required,
        status: ResponseStatus::NeedsAction,
    }];
    meeting.own_response = OwnResponse::NeedsAction;
    let holiday = event(
        "ev-holiday",
        "cal-second",
        "holiday@example.com",
        "Holiday",
        EventTime::all_day((day(5)).date_naive(), (day(7)).date_naive()).unwrap(),
    );
    json!({
        "calendars": [calendar("cal-main", "Calendar", true), calendar("cal-second", "Holidays", false)],
        "events": [meeting, holiday],
        "log": log.to_string_lossy(),
        "missing_scope": missing_scope,
        "message_events": {INVITE_GRAPH_ID: "ev-meeting"},
    })
    .to_string()
}

fn request_ics(uid: &str, summary: &str, start: DateTime<Utc>, attendee: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nPRODID:-//mailypoppins//calendar slice//EN\r\nVERSION:2.0\r\n\
         METHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\n\
         DTSTAMP:20260101T000000Z\r\nSUMMARY:{summary}\r\nDTSTART:{}\r\nDTEND:{}\r\n\
         LOCATION:Room 4.12\r\nORGANIZER:mailto:chair@tum.de\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n",
        ics_time(start),
        ics_time(start + Days::hours(1)),
    )
    .into_bytes()
}

/// The configuration and one invitation mail per account: the server
/// account's is about the server's meeting, and is bound to a Graph id.
fn seed(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("config.toml"), CONFIG).unwrap();

    let mut mail = read_fixture::email(
        "Ada <ada@example.com>",
        "sylvain@example.com",
        "Invitation: Project review",
        "Mon, 6 Jul 2026 10:00:00 +0200",
        "see you there\n",
    );
    mail.message_id = Some("<meeting@example.com>".into());
    mail.calendar_ics = Some(request_ics(
        MEETING_UID,
        "Project review",
        at(3, 9),
        "sylvain@example.com",
    ));
    read_fixture::ingest(root, SERVER, "inbox", 1, &mail);
    read_fixture::store(root, SERVER)
        .conn()
        .execute(
            "UPDATE messages SET graph_id = ?1 WHERE message_id LIKE '%meeting@example.com%'",
            [INVITE_GRAPH_ID],
        )
        .unwrap();

    let mut local = read_fixture::email(
        "Chair <chair@tum.de>",
        "perso@example.com",
        "Invitation: LOC Day planning",
        "Mon, 6 Jul 2026 10:00:00 +0200",
        "please let me know\n",
    );
    local.message_id = Some("<loc-day@example.com>".into());
    local.calendar_ics = Some(request_ics(
        LOCAL_UID,
        "LOC Day planning",
        at(4, 12),
        "perso@example.com",
    ));
    read_fixture::ingest(root, IMIP, "inbox", 1, &local);
}

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
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let spec = fake_spec(&tmp.path().join("calendar.jsonl"), missing_scope);
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

/// An account without a backend views exactly its agenda: the rows of
/// `calendar.events`, at the same instants, addressed `imip:<row_id>`.
#[tokio::test]
async fn a_none_accounts_view_is_its_imip_agenda() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let agenda = call(&mut conn, "calendar.events", json!({"account": IMIP})).await;
    let agenda = agenda["events"].as_array().unwrap().clone();
    let events = view(&mut conn, json!({"accounts": [IMIP]})).await;
    assert_eq!(agenda.len(), 1);
    assert_eq!(events.len(), agenda.len());
    for (row, event) in agenda.iter().zip(&events) {
        assert_eq!(event["id"], format!("imip:{}", row["row_id"]));
        assert_eq!(event["calendar"], "imip");
        assert_eq!(event["uid"], row["event"]["uid"]);
        assert_eq!(event["summary"], row["event"]["summary"]);
        assert_eq!(event["location"], row["event"]["location"]);
        assert_eq!(
            event["time"]["start"],
            format!("{}Z", row["start_sort"].as_str().unwrap())
        );
        assert_eq!(event["own_response"], "needs-action");
        assert_eq!(event["source"]["row_id"], row["row_id"]);
        assert_eq!(event["source"]["subject"], row["subject"]);
        assert_eq!(
            event["source"]["selector"],
            "mp://perso/inbox/loc-day@example.com"
        );
    }

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
