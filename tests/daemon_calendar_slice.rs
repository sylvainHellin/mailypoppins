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

use mailypoppins::calendar_backend::fake::{RecordedResponse, FAKE_CALENDAR_ENV};
use mailypoppins::calendar_backend::Response;
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use support::calendar_fixture::{
    day, fake_spec_with, seed, seed_series, series_occurrences, IMIP, LOCAL_UID, MEETING_UID,
    SERIES_UID, SERVER,
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
