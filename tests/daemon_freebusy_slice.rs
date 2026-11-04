//! `calendar.freebusy` over the daemon (PERSO-255, phase 2, slice 7).
//!
//! Every row runs a real `mp daemon run` against a sandboxed root with the
//! calendar fake armed through `MAILYPOPPINS_DAEMON_FAKE_CALENDAR`, its spec
//! the calendar slice's plus `free-busy` and the busy blocks of two
//! addresses:
//!
//! - the query asks the account's own address first, answers each schedule
//!   with its view and blocks, and an address the server has no free/busy
//!   for with its error, the others standing;
//! - `calendar.list` reports `free-busy` on the account's calendars;
//! - an iMIP-only account, a backend without `free-busy`, a range or an
//!   address the server would refuse, and a sign-in without the calendar
//!   scope are refused before an operation starts;
//! - the query is a client-scoped operation.

mod support;

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::{FAKE_CALENDAR_ENV, FAKE_NO_FREE_BUSY};
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::calendar::CalendarFreeBusyResult;
use mp_protocol::RpcError;

use support::calendar_fixture::{
    at, fake_spec_free_busy, seed, COLLEAGUE as ADA, IMIP, OWN_ADDRESS as OWN, SERVER,
};
use support::parity::{socket_path, DaemonFixture};

const DEADLINE: Duration = Duration::from_secs(20);

/// An address outside the organisation, which has none.
const OUTSIDE: &str = "someone@gmail.com";

struct Slice {
    tmp: TempDir,
    daemon: Option<DaemonFixture>,
}

impl Slice {
    fn start(missing_scope: bool, free_busy: bool) -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let spec =
            fake_spec_free_busy(&tmp.path().join("calendar.jsonl"), missing_scope, free_busy);
        let daemon =
            DaemonFixture::start_with(tmp.path(), None, &[(FAKE_CALENDAR_ENV, spec.as_str())]);
        Slice {
            tmp,
            daemon: Some(daemon),
        }
    }

    async fn connect(&self) -> Connection {
        let root = self.tmp.path();
        let mut conn = within("connect", Connection::connect(&socket_path(root)))
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
                    data_dir: root.to_path_buf(),
                    config_dir: root.to_path_buf(),
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

/// The terminal status of the operation `started` names.
async fn settled(conn: &mut Connection, started: &Value) -> Value {
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
        assert!(
            start.elapsed() < DEADLINE,
            "calendar.freebusy never settled"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The day three days out, as the fake's blocks are dated, in UTC.
fn day_params(addresses: &[&str]) -> Value {
    let day = at(3, 0).date_naive().to_string();
    json!({
        "account": SERVER,
        "addresses": addresses,
        "start": day,
        "end": day,
        "zone": "UTC",
    })
}

/// The account's own schedule comes first, then each address once, Ada's
/// blocks in start order with her view, and the outside address with no
/// free/busy, which does not fail the query.
#[tokio::test]
async fn calendar_freebusy_answers_the_account_and_each_address() {
    let slice = Slice::start(false, true);
    let mut conn = slice.connect().await;
    let started = call(
        &mut conn,
        "calendar.freebusy",
        day_params(&[ADA, OUTSIDE, "ADA@example.com"]),
    )
    .await;
    let status = settled(&mut conn, &started).await;
    assert_eq!(status["state"], "succeeded", "{status}");
    assert_eq!(status["scope"], "client_scoped", "{status}");
    let result: CalendarFreeBusyResult =
        serde_json::from_value(status["result"].clone()).expect("the settled result");
    assert_eq!(result.account, SERVER);
    assert_eq!(result.interval_minutes, 30);
    assert_eq!(
        result.start,
        at(3, 0).format("%Y-%m-%dT%H:%M:%SZ").to_string()
    );
    assert_eq!(
        result.end,
        at(4, 0).format("%Y-%m-%dT%H:%M:%SZ").to_string()
    );
    let addresses: Vec<&str> = result
        .schedules
        .iter()
        .map(|s| s.address.as_str())
        .collect();
    assert_eq!(addresses, [OWN, ADA, OUTSIDE]);

    let own = &result.schedules[0];
    assert_eq!(own.view.len(), 48);
    assert_eq!(&own.view[18..20], "22", "09:00 to 10:00 busy");
    assert_eq!(own.view.matches('2').count(), 2);
    assert_eq!(own.items[0].subject.as_deref(), Some("Project review"));

    let ada = &result.schedules[1];
    assert_eq!(&ada.view[18..22], "3333", "away 09:00 to 11:00");
    assert_eq!(&ada.view[26..28], "11", "tentative 13:00 to 14:00");
    let statuses: Vec<&str> = ada.items.iter().map(|i| i.status.as_str()).collect();
    assert_eq!(statuses, ["oof", "tentative"]);

    let outside = &result.schedules[2];
    assert_eq!(outside.error.as_deref(), Some(FAKE_NO_FREE_BUSY));
    assert!(outside.view.is_empty() && outside.items.is_empty());
}

/// `calendar.list` reports `free-busy` on the account's server calendars
/// once the backend serves it, so the forms show the availability rows.
#[tokio::test]
async fn calendar_list_reports_free_busy() {
    let slice = Slice::start(false, true);
    let mut conn = slice.connect().await;
    let rebuilt = call(&mut conn, "calendar.rebuild", json!({"account": SERVER})).await;
    settled(&mut conn, &rebuilt).await;
    let listed = call(&mut conn, "calendar.list", json!({"account": SERVER})).await;
    let calendars = listed["calendars"].as_array().unwrap();
    let server: Vec<&Value> = calendars
        .iter()
        .filter(|c| c["backend"] != "none")
        .collect();
    assert!(!server.is_empty(), "{listed}");
    for calendar in server {
        let features: Vec<&str> = calendar["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        assert!(features.contains(&"free-busy"), "{calendar}");
    }
}

/// An iMIP-only account, a backend without free/busy, a range or a slot
/// the server would refuse and an address that is none are refused with
/// their sentence before an operation starts.
#[tokio::test]
async fn calendar_freebusy_refuses_before_an_operation_starts() {
    let slice = Slice::start(false, true);
    let mut conn = slice.connect().await;
    let mut imip = day_params(&[ADA]);
    imip["account"] = IMIP.into();
    let refused = call_err(&mut conn, "calendar.freebusy", imip).await;
    assert_eq!(refused.code, -32602);
    assert!(
        refused.message.contains("no calendar backend"),
        "{}",
        refused.message
    );

    let mut long = day_params(&[ADA]);
    long["end"] = at(70, 0).date_naive().to_string().into();
    let refused = call_err(&mut conn, "calendar.freebusy", long).await;
    assert_eq!(refused.code, -32602);
    assert!(refused.message.contains("62 days"), "{}", refused.message);

    let mut slot = day_params(&[ADA]);
    slot["interval_minutes"] = 2.into();
    let refused = call_err(&mut conn, "calendar.freebusy", slot).await;
    assert!(refused.message.contains("5 to 1440"), "{}", refused.message);

    let refused = call_err(&mut conn, "calendar.freebusy", day_params(&["ada"])).await;
    assert_eq!(refused.code, -32602);
    assert!(
        refused.message.contains("not an email address"),
        "{}",
        refused.message
    );

    let many: Vec<String> = (0..20).map(|i| format!("p{i}@example.com")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let refused = call_err(&mut conn, "calendar.freebusy", day_params(&many)).await;
    assert!(
        refused.message.contains("at most 20"),
        "{}",
        refused.message
    );

    let slice = Slice::start(false, false);
    let mut conn = slice.connect().await;
    let refused = call_err(&mut conn, "calendar.freebusy", day_params(&[ADA])).await;
    assert_eq!(
        (refused.code, refused.message.as_str()),
        (-32602, "The calendar backend of evoqs offers no free/busy")
    );
}

/// A sign-in without the calendar scope is the re-login prompt, before an
/// operation starts.
#[tokio::test]
async fn calendar_freebusy_without_the_scope_is_the_relogin_prompt() {
    let slice = Slice::start(true, true);
    let mut conn = slice.connect().await;
    let refused = call_err(&mut conn, "calendar.freebusy", day_params(&[ADA])).await;
    assert_eq!(refused.code, -32603);
    assert_eq!(
        refused.message,
        format!("calendar needs a re-login (mp config oauth2-login -A {SERVER})")
    );
    assert_eq!(
        refused.data.as_ref().unwrap()["missing_scopes"],
        json!(["Calendars.ReadWrite"])
    );
}
