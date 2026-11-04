//! The calendar reminders over the daemon (PERSO-255, calendar phase 2).
//!
//! Every row runs a real `mp daemon run` against a sandboxed root, with the
//! calendar fake armed through `MAILYPOPPINS_DAEMON_FAKE_CALENDAR` (serving
//! `server-reminders`) and the reminder clock running sixty times faster than
//! real time from the daemon's start (`MAILYPOPPINS_DAEMON_FAKE_CLOCK_SPEED`),
//! so a snooze of a minute ends within a second:
//!
//! - an overdue reminder whose event has not ended fires once the calendar
//!   synced, as `calendar.reminder` to every client, with the desktop app as
//!   its presenter, and `calendar.reminders` lists it;
//! - a snooze takes it off the list, says so with `calendar.reminders_changed`,
//!   and fires it again when it ends; a dismissal takes it off for good, and a
//!   `calendar.rebuild` does not bring it back;
//! - both decisions reach the server through the fake;
//! - the presenter follows the clients connected: the desktop app first, else
//!   a TUI, else none;
//! - an event without a reminder, a snooze out of range and a snooze of a
//!   dismissed reminder are refused.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{Duration as Minutes, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::{RecordedWrite, FAKE_CALENDAR_ENV};
use mailypoppins::calendar_model::{EventTime, OwnResponse};
use mailypoppins::daemon::reminders::FAKE_CLOCK_SPEED_ENV;
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use support::calendar_fixture::{event, fake_spec_with, seed, SERVER};
use support::parity::{socket_path, DaemonFixture};

const DEADLINE: Duration = Duration::from_secs(20);
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// A seeded root and its daemon.
struct Slice {
    tmp: TempDir,
    daemon: Option<DaemonFixture>,
}

impl Slice {
    /// The fake serves `server-reminders` and holds, beside the fixture's
    /// events, a standup that started five minutes ago and reminds ten minutes
    /// ahead (overdue, not ended), and a call without a reminder.
    fn start() -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let now = Utc::now();
        let mut standup = event(
            "ev-standup",
            "cal-main",
            "standup@example.com",
            "Standup",
            EventTime::timed(now - Minutes::minutes(5), now + Minutes::hours(6), "UTC").unwrap(),
        );
        standup.reminder = Some(10);
        standup.own_response = OwnResponse::Accepted;
        standup.location = Some("Room 2.14".into());
        let quiet = event(
            "ev-quiet",
            "cal-main",
            "quiet@example.com",
            "Quiet call",
            EventTime::timed(now + Minutes::hours(1), now + Minutes::hours(2), "UTC").unwrap(),
        );
        let mut spec: Value = serde_json::from_str(&fake_spec_with(
            &tmp.path().join("calendar.jsonl"),
            false,
            vec![standup, quiet],
        ))
        .unwrap();
        spec["features"] = json!(["server-reminders"]);
        spec["write_log"] = json!(tmp.path().join("writes.jsonl").to_string_lossy());
        let spec = spec.to_string();
        let env = [
            (FAKE_CALENDAR_ENV, spec.as_str()),
            (FAKE_CLOCK_SPEED_ENV, "60"),
        ];
        let daemon = DaemonFixture::start_with(tmp.path(), None, &env);
        Slice {
            tmp,
            daemon: Some(daemon),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn writes(&self) -> Vec<RecordedWrite> {
        match std::fs::read_to_string(self.root().join("writes.jsonl")) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    async fn connect(&self, kind: ClientKind) -> Connection {
        let mut conn = within("connect", Connection::connect(&socket_path(self.root())))
            .await
            .expect("connect");
        within(
            "initialize",
            conn.initialize(
                ClientInfo {
                    kind,
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

    /// A connection of `kind` that bootstrapped, which is what a TUI or a
    /// desktop app is to the presenter rule.
    async fn attach(&self, kind: ClientKind) -> Connection {
        let mut conn = self.connect(kind).await;
        call(&mut conn, "state.bootstrap", json!({})).await;
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

/// Start an operation and wait for it to succeed.
async fn run_operation(conn: &mut Connection, method: &str, params: Value) -> Value {
    let started = call(conn, method, params).await;
    let id = started["operation_id"].as_str().unwrap().to_string();
    let start = Instant::now();
    loop {
        let status = call(conn, "operation.status", json!({"operation_id": id})).await;
        match status["state"].as_str() {
            Some("succeeded") => return status["result"].clone(),
            Some("failed" | "cancelled") => panic!("{method} did not succeed: {status}"),
            _ => {}
        }
        assert!(start.elapsed() < SETTLE_DEADLINE, "{method} never settled");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The payload of the next `kind` event `watcher` receives.
async fn next_event(watcher: &mut Connection, kind: &str) -> Value {
    let start = Instant::now();
    while start.elapsed() < SETTLE_DEADLINE {
        let Some(notification) = within(kind, watcher.next_notification()).await else {
            break;
        };
        if notification.params["kind"] == kind {
            return notification.params["payload"].clone();
        }
    }
    panic!("no {kind} reached the client");
}

async fn pending(conn: &mut Connection) -> Vec<Value> {
    let answer = call(conn, "calendar.reminders", json!({})).await;
    answer["reminders"].as_array().unwrap().clone()
}

/// Ask `calendar.reminders` until its presenter reads `expected`: a
/// disconnect reaches the daemon a moment after the socket closes.
async fn presenter_becomes(conn: &mut Connection, expected: Value) {
    let start = Instant::now();
    loop {
        let listed = pending(conn).await;
        if listed[0]["presenter"] == expected {
            return;
        }
        assert!(
            start.elapsed() < SETTLE_DEADLINE,
            "the presenter stayed {} instead of {expected}",
            listed[0]["presenter"]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Fire, snooze, fire again, dismiss, and the server told of both decisions.
#[tokio::test]
async fn a_reminder_fires_snoozes_and_dismisses() {
    let slice = Slice::start();
    let mut gui = slice.attach(ClientKind::Gui).await;
    let mut conn = slice.connect(ClientKind::Cli).await;
    assert!(pending(&mut conn).await.is_empty(), "nothing synced yet");

    run_operation(&mut conn, "calendar.rebuild", json!({"account": SERVER})).await;
    let fired = next_event(&mut gui, "calendar.reminder").await;
    assert_eq!(fired["account"], SERVER);
    assert_eq!(fired["event"], "ev-standup");
    assert_eq!(fired["summary"], "Standup");
    assert_eq!(fired["location"], "Room 2.14");
    assert_eq!(fired["minutes_before"], 10);
    assert_eq!(fired["all_day"], false);
    assert_eq!(fired["presenter"], "gui");
    let listed = pending(&mut conn).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["event"], "ev-standup");
    assert_eq!(listed[0]["presenter"], "gui");

    let snoozed = call(
        &mut conn,
        "calendar.snooze_reminder",
        json!({"account": SERVER, "event": "ev-standup", "minutes": 1}),
    )
    .await;
    assert_eq!(snoozed["event"], "ev-standup");
    assert!(
        snoozed["until"].as_str().unwrap().ends_with('Z'),
        "{snoozed}"
    );
    assert_eq!(
        next_event(&mut gui, "calendar.reminders_changed").await["account"],
        SERVER
    );
    assert!(
        pending(&mut conn).await.is_empty(),
        "a snoozed reminder waits"
    );
    let again = next_event(&mut gui, "calendar.reminder").await;
    assert_eq!(again["event"], "ev-standup", "the snooze ended");
    assert_eq!(pending(&mut conn).await.len(), 1);

    let dismissed = call(
        &mut conn,
        "calendar.dismiss_reminder",
        json!({"account": SERVER, "event": "ev-standup"}),
    )
    .await;
    assert_eq!(dismissed, json!({"account": SERVER, "event": "ev-standup"}));
    next_event(&mut gui, "calendar.reminders_changed").await;
    assert!(pending(&mut conn).await.is_empty());
    let refused = call_err(
        &mut conn,
        "calendar.snooze_reminder",
        json!({"account": SERVER, "event": "ev-standup", "minutes": 5}),
    )
    .await;
    assert_eq!(refused.code, -32602);
    assert!(refused.message.contains("dismissed"), "{refused:?}");

    // A rebuild drops the cache and keeps the record: nothing fires again.
    run_operation(&mut conn, "calendar.rebuild", json!({"account": SERVER})).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        pending(&mut conn).await.is_empty(),
        "the dismissal survived"
    );

    let start = Instant::now();
    let decided = loop {
        let decided: Vec<RecordedWrite> = slice
            .writes()
            .into_iter()
            .filter(|write| {
                matches!(
                    write,
                    RecordedWrite::DismissReminder { .. } | RecordedWrite::SnoozeReminder { .. }
                )
            })
            .collect();
        if decided.len() == 2 || start.elapsed() > SETTLE_DEADLINE {
            break decided;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        matches!(&decided[..], [
            RecordedWrite::SnoozeReminder { event: snoozed, .. },
            RecordedWrite::DismissReminder { event: dismissed },
        ] if snoozed == "ev-standup" && dismissed == "ev-standup"),
        "{decided:?}"
    );
}

/// The presenter is the desktop app while one is connected, else a TUI,
/// else none.
#[tokio::test]
async fn the_presenter_follows_the_clients_connected() {
    let slice = Slice::start();
    let gui = slice.attach(ClientKind::Gui).await;
    let tui = slice.attach(ClientKind::Tui).await;
    let mut conn = slice.connect(ClientKind::Cli).await;
    run_operation(&mut conn, "calendar.rebuild", json!({"account": SERVER})).await;
    let start = Instant::now();
    while pending(&mut conn).await.is_empty() {
        assert!(
            start.elapsed() < SETTLE_DEADLINE,
            "the reminder never fired"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    presenter_becomes(&mut conn, json!("gui")).await;
    drop(gui);
    presenter_becomes(&mut conn, json!("tui")).await;
    drop(tui);
    presenter_becomes(&mut conn, Value::Null).await;
}

/// What cannot be dismissed or snoozed is refused before anything is
/// recorded.
#[tokio::test]
async fn the_refusals() {
    let slice = Slice::start();
    let mut conn = slice.connect(ClientKind::Cli).await;
    run_operation(&mut conn, "calendar.rebuild", json!({"account": SERVER})).await;
    for (method, params, says) in [
        (
            "calendar.dismiss_reminder",
            json!({"account": SERVER, "event": "ev-quiet"}),
            "has no reminder",
        ),
        (
            "calendar.dismiss_reminder",
            json!({"account": SERVER, "event": "ev-nowhere"}),
            "has no event",
        ),
        (
            "calendar.snooze_reminder",
            json!({"account": SERVER, "event": "ev-standup", "minutes": 0}),
            "minutes",
        ),
        (
            "calendar.snooze_reminder",
            json!({"account": SERVER, "event": "ev-standup"}),
            "minutes",
        ),
        (
            "calendar.reminders",
            json!({"account": SERVER}),
            "has no account parameter",
        ),
    ] {
        let refused = call_err(&mut conn, method, params).await;
        assert_eq!(refused.code, -32602, "{method}: {refused:?}");
        assert!(refused.message.contains(says), "{method}: {refused:?}");
    }
    assert!(slice.writes().is_empty(), "nothing reached the server");
}
