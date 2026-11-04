//! The server calendar's shared fixture (PERSO-200): two accounts, one whose
//! `calendar_backend` the calendar fake stands in for and one without, the
//! fake's calendars and events, and one invitation mail per account.
//!
//! [`seed`] writes the configuration and the mails; [`fake_spec`] is the
//! value of `MAILYPOPPINS_DAEMON_FAKE_CALENDAR` the daemon is started with.
//! The events are dated from today, because the daemon caches a window
//! around the real clock (`calendar_model::window_for`).

use std::path::Path;

use chrono::{DateTime, Duration as Days, NaiveTime, Utc};
use serde_json::json;

use mailypoppins::calendar_backend::SeriesInfo;
use mailypoppins::calendar_model::{
    Attendee, AttendeeRole, Calendar, Event, EventKind, EventTime, Frequency,
    OnlineMeetingProvider, OwnResponse, Person, Recurrence, ResponseStatus, Sensitivity, ShowAs,
};

use super::read_fixture;

/// The account with a calendar backend: a password account that names
/// `graph`, which the fake stands in for.
pub const SERVER: &str = "evoqs";
/// The account without one.
pub const IMIP: &str = "perso";

/// The `UID` the server's meeting and the invitation mail about it share.
pub const MEETING_UID: &str = "meeting-1@example.com";
/// The `UID` of the invitation only the account without a backend holds.
pub const LOCAL_UID: &str = "loc-day@tum.de";
/// The Graph id the server account's invitation row is bound to.
pub const INVITE_GRAPH_ID: &str = "AAMkAD-invite";
/// The `UID` of the weekly series [`seed_series`] invites the server account
/// to and [`series_occurrences`] holds on the server.
pub const SERIES_UID: &str = "weekly-sync@example.com";

pub const CONFIG: &str = r#"
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

/// Midnight UTC `days` from today.
pub fn day(days: i64) -> DateTime<Utc> {
    (Utc::now().date_naive() + Days::days(days))
        .and_time(NaiveTime::MIN)
        .and_utc()
}

/// `days` from today at `hour` UTC.
pub fn at(days: i64, hour: i64) -> DateTime<Utc> {
    day(days) + Days::hours(hour)
}

pub fn ics_time(instant: DateTime<Utc>) -> String {
    instant.format("%Y%m%dT%H%M%SZ").to_string()
}

pub fn calendar(id: &str, name: &str, is_default: bool) -> Calendar {
    Calendar {
        account: "spec".into(),
        id: id.into(),
        name: name.into(),
        color: Some("#3b82f6".into()),
        is_default,
        can_edit: true,
        visible: true,
        online_meeting_providers: vec![OnlineMeetingProvider::TeamsForBusiness],
    }
}

pub fn event(id: &str, calendar: &str, uid: &str, summary: &str, time: EventTime) -> Event {
    Event {
        account: "spec".into(),
        calendar: calendar.into(),
        id: id.into(),
        uid: uid.into(),
        recurrence_id: None,
        series_master_id: None,
        kind: EventKind::Single,
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
        is_online_meeting: false,
        online_meeting_provider: None,
        sensitivity: Sensitivity::Normal,
        show_as: ShowAs::Busy,
        cancelled: false,
        is_organizer: false,
        last_modified: None,
        web_link: None,
        allow_new_time_proposals: true,
        response_requested: true,
        transaction_id: None,
    }
}

/// Where the fake whose answer log is `log` records its writes.
pub fn write_log(log: &Path) -> std::path::PathBuf {
    log.with_extension("writes.jsonl")
}

/// The fake's server: a default calendar that offers Teams and a second one
/// that offers no online meeting (as a mailbox without a Teams licence), a
/// meeting the server account was also invited to by mail, and an all-day
/// holiday.
pub fn fake_spec(log: &Path, missing_scope: bool) -> String {
    fake_spec_with(log, missing_scope, Vec::new())
}

/// [`fake_spec`] with `extra` events on the server beside the meeting and the
/// holiday.
pub fn fake_spec_with(log: &Path, missing_scope: bool, extra: Vec<Event>) -> String {
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
        proposed_time: None,
    }];
    meeting.own_response = OwnResponse::NeedsAction;
    let holiday = event(
        "ev-holiday",
        "cal-second",
        "holiday@example.com",
        "Holiday",
        EventTime::all_day((day(5)).date_naive(), (day(7)).date_naive()).unwrap(),
    );
    let events = [vec![meeting, holiday], extra].concat();
    let mut second = calendar("cal-second", "Holidays", false);
    second.online_meeting_providers = Vec::new();
    json!({
        "calendars": [calendar("cal-main", "Calendar", true), second],
        "events": events,
        "log": log.to_string_lossy(),
        "write_log": write_log(log).to_string_lossy(),
        "missing_scope": missing_scope,
        "message_events": {INVITE_GRAPH_ID: "ev-meeting"},
        "features": FAKE_FEATURES,
        "series": [series_info()],
    })
    .to_string()
}

pub fn request_ics(uid: &str, summary: &str, start: DateTime<Utc>, attendee: &str) -> Vec<u8> {
    request_ics_with(uid, summary, start, attendee, "")
}

/// [`request_ics`] with `extra` content lines in the `VEVENT`, each ending in
/// `\r\n`.
fn request_ics_with(
    uid: &str,
    summary: &str,
    start: DateTime<Utc>,
    attendee: &str,
    extra: &str,
) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nPRODID:-//mailypoppins//calendar slice//EN\r\nVERSION:2.0\r\n\
         METHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\n\
         DTSTAMP:20260101T000000Z\r\nSUMMARY:{summary}\r\nDTSTART:{}\r\nDTEND:{}\r\n{extra}\
         LOCATION:Room 4.12\r\nORGANIZER:mailto:chair@tum.de\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n",
        ics_time(start),
        ics_time(start + Days::hours(1)),
    )
    .into_bytes()
}

/// What the fake claims to serve, which `calendar.list` reports on its
/// calendars that offer Teams; the others lack `online-meeting`.
pub const FAKE_FEATURES: [&str; 3] = ["write", "online-meeting", "propose-time"];

/// The weekly series of [`series_occurrences`] as the fake's baseline
/// resolves it: its master, a Teams meeting, and its rule of two weeks.
pub fn series_info() -> SeriesInfo {
    let start = at(10, 8);
    let mut master = event(
        "ev-series",
        "cal-main",
        SERIES_UID,
        "Weekly sync",
        EventTime::timed(start, start + Days::hours(1), "Europe/Berlin").unwrap(),
    );
    master.kind = EventKind::SeriesMaster;
    master.is_online_meeting = true;
    master.online_meeting_provider = Some(OnlineMeetingProvider::TeamsForBusiness);
    SeriesInfo {
        master_id: "ev-series".into(),
        uid: SERIES_UID.into(),
        recurrence: Some(Recurrence {
            frequency: Frequency::Weekly,
            interval: 1,
            weekdays: Vec::new(),
            day_of_month: None,
            week_index: None,
            month: None,
            first_day_of_week: None,
            start: Some(start.date_naive()),
            until: None,
            count: Some(2),
        }),
        master: Some(Box::new(master)),
        zone: Some("Europe/Berlin".into()),
    }
}

/// The two server occurrences of the weekly series, ten and seventeen days
/// out, each named by its original start the way Graph names one; the server
/// never lists the series master. They carry what a backend gives an
/// occurrence: the series' rule in words and its Teams meeting.
pub fn series_occurrences() -> Vec<Event> {
    [("ev-series-1", 10), ("ev-series-2", 17)]
        .into_iter()
        .map(|(id, days)| {
            let start = at(days, 8);
            let mut occurrence = event(
                id,
                "cal-main",
                SERIES_UID,
                "Weekly sync",
                EventTime::timed(start, start + Days::hours(1), "Europe/Berlin").unwrap(),
            );
            occurrence.recurrence_id = Some(start.to_rfc3339());
            occurrence.series_master_id = Some("ev-series".into());
            occurrence.kind = EventKind::Occurrence;
            occurrence.recurrence = Some("Weekly (2 times)".into());
            occurrence.is_online_meeting = true;
            occurrence.online_meeting_provider = Some(OnlineMeetingProvider::TeamsForBusiness);
            occurrence
        })
        .collect()
}

/// A second invitation mail on the server account: the request for the whole
/// weekly series, with an `RRULE` and no `RECURRENCE-ID`.
pub fn seed_series(root: &Path) {
    let mut mail = read_fixture::email(
        "Ada <ada@example.com>",
        "sylvain@example.com",
        "Invitation: Weekly sync",
        "Mon, 6 Jul 2026 11:00:00 +0200",
        "every week\n",
    );
    mail.message_id = Some("<weekly-sync@example.com>".into());
    mail.calendar_ics = Some(request_ics_with(
        SERIES_UID,
        "Weekly sync",
        at(10, 8),
        "sylvain@example.com",
        "RRULE:FREQ=WEEKLY;COUNT=2\r\n",
    ));
    read_fixture::ingest(root, SERVER, "inbox", 2, &mail);
}

/// The configuration and one invitation mail per account: the server
/// account's is about the server's meeting, and is bound to a Graph id.
pub fn seed(root: &Path) {
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

/// The account's own address, its `default_from`, which free/busy asks
/// about first.
pub const OWN_ADDRESS: &str = "sylvain@example.com";
/// A colleague [`fake_spec_free_busy`] has free/busy for.
pub const COLLEAGUE: &str = "ada@example.com";

/// [`fake_spec`] with `free-busy` among the fake's features when
/// `free_busy` is set (PERSO-255), and the busy blocks three days out of
/// the account (09:00 to 10:00 UTC, busy) and of [`COLLEAGUE`] (09:00 to
/// 11:00 away, 13:00 to 14:00 tentative), the colleague's address in
/// another case than a client types it.
pub fn fake_spec_free_busy(log: &Path, missing_scope: bool, free_busy: bool) -> String {
    let mut spec: serde_json::Value =
        serde_json::from_str(&fake_spec(log, missing_scope)).expect("the fake spec");
    if free_busy {
        spec["features"]
            .as_array_mut()
            .expect("the features")
            .push("free-busy".into());
    }
    let block = |from: i64, to: i64, status: &str, subject: Option<&str>| {
        let mut item = json!({
            "start": at(3, from).to_rfc3339(),
            "end": at(3, to).to_rfc3339(),
            "status": status,
        });
        if let Some(subject) = subject {
            item["subject"] = subject.into();
        }
        item
    };
    spec["free_busy"] = json!({
        OWN_ADDRESS: [block(9, 10, "busy", Some("Project review"))],
        "Ada@Example.com": [
            block(13, 14, "tentative", None),
            block(9, 11, "oof", Some("Away")),
        ],
    });
    spec.to_string()
}
