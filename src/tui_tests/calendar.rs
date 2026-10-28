//! The TUI's Calendar view (PERSO-200): its frames, and its reads and writes
//! through the daemon.
//!
//! The frames are built from the frozen fixture of
//! [`crate::tui::ui::golden_frames::calendar_fixture`], a week of fixed dates
//! in a pinned zone, because a frame that printed a date read off the clock
//! would change every day. The agenda's own frame is the fixture's
//! `golden_calendar_view`, which [`super::golden_frames_daemon`] renders from
//! a bootstrap too.
//!
//! The daemon half runs against a store whose server-calendar cache was
//! filled by [`sync_account`] from an in-memory [`FakeCalendarBackend`] on
//! the test's own thread. The daemon's `calendar.rebuild` and `event.respond`
//! are not driven here: both run on a thread of their own that does not carry
//! the test's data-root override, so they would resolve the developer's own
//! store; `tests/daemon_calendar_slice.rs` drives them against a daemon
//! process instead, and the respond row below stops at the call the TUI makes.

use std::sync::Mutex;

use chrono::{DateTime, Datelike, Days, NaiveTime, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use insta::assert_snapshot;
use serde_json::{json, Value};

use crate::calendar_backend::fake::FakeCalendarBackend;
use crate::calendar_model::{
    Attendee, AttendeeRole, Calendar, Event, EventTime, OwnResponse, Person, ResponseStatus,
    Sensitivity, ShowAs,
};
use crate::calendar_sync::sync_account;
use crate::config::{AccountConfig, CalendarBackendKind};
use crate::tui::app::{
    AccountState, Action, App, BgResult, CalendarLayout, Credentials, Overlay, RsvpChoice,
    RsvpOverlay, RsvpRow, View,
};
use crate::tui::commands::{dispatch, settled};
use crate::tui::events::{Applied, Awaited};
use crate::tui::queries::Queries;
use crate::tui::ui::golden_frames::{calendar_fixture, frame_snapshot, HEIGHT, WIDTH};

use super::daemon::{CallLog, TestDaemon};

// ---------------------------------------------------------------------------
// The frames
// ---------------------------------------------------------------------------

/// The fixture in `layout`, anchored on `anchor` (`YYYY-MM-DD`), the cursor
/// on its `index`th row.
fn fixture_in(layout: CalendarLayout, anchor: &str, index: usize) -> App {
    let mut app = calendar_fixture();
    app.calendar_view.layout = layout;
    app.calendar_view.anchor = chrono::NaiveDate::parse_from_str(anchor, "%Y-%m-%d").unwrap();
    app.recompute_calendar_entries(None);
    app.calendar_view.list_index = index;
    app
}

/// The agenda in a column too narrow for one-line rows: each event's
/// `account/calendar` tag and place on a second line, all-day rows first on
/// their day, the card beside it.
#[test]
fn golden_calendar_agenda_two_line_rows() {
    let mut app = fixture_in(CalendarLayout::Agenda, "2026-08-03", 1);
    assert_snapshot!(frame_snapshot(&mut app, 100, 30));
}

/// The day grid of the Tuesday: the jour fixe and the overlapping colloquium
/// side by side on the hour grid, the card beside it, and the re-login banner
/// of an account whose sign-in lacks the calendar scope.
#[test]
fn golden_calendar_day() {
    let mut app = fixture_in(CalendarLayout::Day, "2026-08-04", 0);
    let mut evoqs = AccountState::for_tests("evoqs");
    evoqs.credentials = Some(Credentials {
        calendar_scope_missing: true,
        ..Credentials::default()
    });
    app.accounts = vec![evoqs];
    assert_snapshot!(frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// The day grid with its all-day band (the Thursday), and on a pane too
/// narrow for two lanes, the overlapping events of the Tuesday stacked.
#[test]
fn golden_calendar_day_all_day_band_and_stacked() {
    let mut app = fixture_in(CalendarLayout::Day, "2026-08-06", 1);
    assert_snapshot!("day_all_day_band", frame_snapshot(&mut app, 90, 30));
    let mut app = fixture_in(CalendarLayout::Day, "2026-08-04", 1);
    assert_snapshot!("day_stacked", frame_snapshot(&mut app, 34, 30));
}

/// The week grid on calcure's model: seven columns with the day numbers,
/// today's highlighted, the all-day band, the two occurrences of the
/// recurring jour fixe, the cancelled meeting struck through, and the event
/// under the cursor in the strip on the right.
#[test]
fn golden_calendar_week() {
    let mut app = fixture_in(CalendarLayout::Week, "2026-08-05", 2);
    assert_snapshot!(frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// A week on a terminal too narrow for seven columns falls back to the day
/// grid of the day under the cursor.
#[test]
fn golden_calendar_week_narrow_falls_back_to_the_day() {
    let mut app = fixture_in(CalendarLayout::Week, "2026-08-05", 5);
    assert_snapshot!(frame_snapshot(&mut app, 70, 30));
}

/// The calendars overlay: every account's calendars, the hidden one
/// unticked.
#[test]
fn golden_calendar_calendars_overlay() {
    let mut app = calendar_fixture();
    app.overlay = Overlay::Calendars(crate::tui::app::CalendarsOverlay {
        calendars: app.calendar_view.calendars.clone(),
        selected: 2,
    });
    assert_snapshot!(frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// The event card of an invitation mail's event, with the mail one key away.
#[test]
fn golden_calendar_event_card() {
    let mut app = fixture_in(CalendarLayout::Agenda, "2026-08-03", 0);
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(app.overlay, Overlay::EventCard(_)));
    assert_snapshot!("event_card_server", frame_snapshot(&mut app, WIDTH, HEIGHT));
    let mut app = fixture_in(CalendarLayout::Agenda, "2026-08-03", 2);
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_snapshot!("event_card_imip", frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// The respond overlay on a server event, with a comment typed and the
/// response not to be sent; and on an invitation mail's event, which has no
/// toggle.
#[test]
fn golden_calendar_respond_overlay() {
    let mut app = calendar_fixture();
    let mut overlay = RsvpOverlay::new(
        "evoqs".to_string(),
        "ev-jour-1".to_string(),
        "Jour fixe".to_string(),
    );
    overlay.selected = 1;
    overlay.row = RsvpRow::Comment;
    overlay.comment = "Bin zehn Minuten sp\u{e4}ter".to_string();
    overlay.send_response = false;
    app.overlay = Overlay::Rsvp(overlay);
    assert_snapshot!("respond_server", frame_snapshot(&mut app, WIDTH, HEIGHT));

    let mut app = calendar_fixture();
    app.overlay = Overlay::Rsvp(RsvpOverlay::new(
        "perso".to_string(),
        "imip:3".to_string(),
        "PhD Kolloquium".to_string(),
    ));
    assert_snapshot!("respond_imip", frame_snapshot(&mut app, WIDTH, HEIGHT));
}

// ---------------------------------------------------------------------------
// Through the daemon
// ---------------------------------------------------------------------------

const ACCOUNT: &str = "evoqs";

/// The account with a server calendar, which the cache below stands in for.
fn account() -> AccountConfig {
    AccountConfig {
        name: ACCOUNT.to_string(),
        default_from: "sylvain@example.com".to_string(),
        calendar_backend: Some(CalendarBackendKind::Graph),
        ..Default::default()
    }
}

/// Midnight UTC `days` from today, `hours` later.
fn at(days: u64, hours: i64) -> DateTime<Utc> {
    (Utc::now().date_naive() + Days::new(days))
        .and_time(NaiveTime::MIN)
        .and_utc()
        + chrono::Duration::hours(hours)
}

fn server_event(id: &str, calendar: &str, summary: &str, time: EventTime) -> Event {
    Event {
        account: ACCOUNT.into(),
        calendar: calendar.into(),
        id: id.into(),
        uid: format!("{id}@example.com"),
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

/// The server: a meeting in the default calendar two days out, and an
/// all-day holiday in a second calendar three days out.
fn server() -> FakeCalendarBackend {
    let calendar = |id: &str, name: &str, is_default: bool| Calendar {
        account: ACCOUNT.into(),
        id: id.into(),
        name: name.into(),
        color: None,
        is_default,
        can_edit: true,
        visible: true,
    };
    let mut meeting = server_event(
        "ev-meeting",
        "cal-main",
        "Project review",
        EventTime::timed(at(2, 9), at(2, 10), "Europe/Berlin").unwrap(),
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
    let holiday = server_event(
        "ev-holiday",
        "cal-second",
        "Holiday",
        EventTime::all_day(at(3, 0).date_naive(), at(4, 0).date_naive()).unwrap(),
    );
    FakeCalendarBackend::new(
        vec![
            calendar("cal-main", "Calendar", true),
            calendar("cal-second", "Holidays", false),
        ],
        vec![meeting, holiday],
    )
}

/// A data root whose store holds the account's server calendar, synced from
/// [`server`] on this thread, where the data-root override is armed.
fn seeded() -> crate::config::test_env::TestDataDir {
    let data = crate::config::test_env::TestDataDir::new();
    std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime
        .block_on(sync_account(
            &store,
            &server().serving(ACCOUNT),
            ACCOUNT,
            Utc::now(),
        ))
        .expect("the fake server syncs");
    data
}

/// An `App` on the account, its Calendar view in UTC from today, reading
/// the daemon through a session that writes every call to `log`.
fn app_over(log: CallLog) -> App {
    let mut app = App::default_for_tests();
    app.account_config = account();
    app.session = Some(TestDaemon::with_accounts(vec![account()]).recording_session(log));
    app.calendar_view.zone = chrono_tz::UTC;
    app.calendar_view.anchor = Utc::now().date_naive();
    app
}

fn summaries(app: &App) -> Vec<String> {
    app.calendar_view
        .events
        .iter()
        .map(|event| event.summary.clone())
        .collect()
}

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

/// The params of every call to `method` in `log`, in order.
fn calls(log: &CallLog, method: &str) -> Vec<Value> {
    log.lock()
        .expect("the call log")
        .iter()
        .filter(|(called, _)| called == method)
        .map(|(_, params)| params.clone())
        .collect()
}

/// The view reads `calendar.view` for its days, and hiding a calendar in the
/// overlay goes through `calendar.set_visibility` and takes its events out
/// of the view; showing it again brings them back.
#[test]
fn hiding_a_calendar_in_the_overlay_removes_its_events() {
    let _data = seeded();
    let log = CallLog::default();
    let mut app = app_over(log.clone());
    app.switch_view(View::Calendar);
    assert_eq!(summaries(&app), ["Project review", "Holiday"]);
    let view = calls(&log, "calendar.view");
    assert_eq!(view.len(), 1, "one read for the first show");
    assert_eq!(view[0]["zone"], "UTC");

    app.handle_key(key('c'));
    let Overlay::Calendars(overlay) = &app.overlay else {
        panic!("c opens the calendars overlay");
    };
    let names: Vec<String> = overlay
        .calendars
        .iter()
        .map(|c| format!("{}/{} {}", c.account, c.name, c.visible))
        .collect();
    assert_eq!(names, ["evoqs/Calendar true", "evoqs/Holidays true"]);

    app.handle_key(key('j'));
    app.handle_key(key(' '));
    assert_eq!(summaries(&app), ["Project review"], "the holiday is hidden");
    assert_eq!(
        calls(&log, "calendar.set_visibility"),
        [json!({"account": ACCOUNT, "calendar": "cal-second", "visible": false})]
    );
    let Overlay::Calendars(overlay) = &app.overlay else {
        panic!("the overlay stays open");
    };
    assert!(!overlay.calendars[1].visible, "the overlay shows it hidden");

    app.handle_key(key(' '));
    assert_eq!(summaries(&app), ["Project review", "Holiday"]);
    app.handle_key(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(app.overlay, Overlay::None));
}

/// Each layout asks `calendar.view` for its own days, from midnight to
/// midnight in the view's zone, and `l`/`h` move them.
#[test]
fn the_layouts_ask_calendar_view_for_their_days() {
    let _data = seeded();
    let log = CallLog::default();
    let mut app = app_over(log.clone());
    app.switch_view(View::Calendar);
    app.handle_key(key('w'));
    let today = Utc::now().date_naive();
    let monday = today - Days::new(u64::from(today.weekday().num_days_from_monday()));
    let midnight = |day: chrono::NaiveDate| format!("{day}T00:00:00+00:00");
    let last = calls(&log, "calendar.view").pop().expect("a read");
    assert_eq!(last["start"], midnight(monday));
    assert_eq!(last["end"], midnight(monday + Days::new(7)));

    app.handle_key(key('l'));
    let last = calls(&log, "calendar.view").pop().expect("a read");
    assert_eq!(last["start"], midnight(monday + Days::new(7)));

    app.handle_key(key('d'));
    app.handle_key(key('t'));
    let last = calls(&log, "calendar.view").pop().expect("a read");
    assert_eq!(last["start"], midnight(today));
    assert_eq!(last["end"], midnight(today + Days::new(1)));
    assert_eq!(app.calendar_view.layout, CalendarLayout::Day);
}

/// `calendar.changed` from the daemon makes the view read its days again:
/// a calendar hidden by another client leaves this one's view.
#[test]
fn a_calendar_changed_event_reloads_the_view() {
    let _data = seeded();
    let mut app = app_over(CallLog::default());
    app.apply_bootstrap(&mp_protocol::state::Bootstrap {
        instance_id: "tui-test".to_string(),
        revision: 1,
        ..Default::default()
    });
    app.switch_view(View::Calendar);
    assert_eq!(summaries(&app), ["Project review", "Holiday"]);

    // Another client hides the second calendar.
    TestDaemon::with_accounts(vec![account()])
        .call(
            "calendar.set_visibility",
            json!({"account": ACCOUNT, "calendar": "cal-second", "visible": false}),
        )
        .expect("the other client's call");
    assert_eq!(
        summaries(&app),
        ["Project review", "Holiday"],
        "not yet told"
    );

    let applied = app.apply_event(&mp_protocol::EventEnvelope {
        instance_id: "tui-test".to_string(),
        revision: 2,
        kind: mp_protocol::events::KIND_CALENDAR_CHANGED.to_string(),
        payload: json!({"account": ACCOUNT}),
    });
    assert_eq!(applied, Applied::Ignored);
    assert_eq!(summaries(&app), ["Project review"]);
}

/// The respond overlay's answer is one `event.respond` with the comment and
/// the send-response choice, on the event's own account; no client-side
/// refusal stands before it, Graph account or not.
#[test]
fn an_answer_is_one_event_respond_with_its_comment_and_choice() {
    #[derive(Default)]
    struct Operations(Mutex<Vec<(String, Value)>>);
    impl Queries for Operations {
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            self.0
                .lock()
                .unwrap()
                .push((method.to_string(), params.clone()));
            Ok(json!({"operation_id": "op-1"}))
        }
    }

    let mut app = App::default_for_tests();
    app.account_config.auth_method = mp_core::config::AuthMethod::Graph;
    let door = Operations::default();
    assert!(dispatch(
        &mut app,
        &door,
        &Action::Rsvp {
            account: ACCOUNT.to_string(),
            event: "ev-meeting".to_string(),
            choice: RsvpChoice::Tentative,
            comment: Some("running late".to_string()),
            send_response: false,
        },
    ));
    let made = door.0.lock().unwrap().clone();
    assert_eq!(
        made,
        [(
            "event.respond".to_string(),
            json!({
                "account": ACCOUNT, "event": "ev-meeting", "response": "tentative",
                "comment": "running late", "send_response": false,
            })
        )]
    );
    assert_eq!(app.bg_count, 1, "the answer is background work");

    let landed = |result: Value| match settled(
        &Awaited::Rsvp { account_index: 0 },
        &json!({"operation_id": "op-1", "state": "succeeded", "result": result}),
    ) {
        BgResult::Rsvp { result, .. } => result,
        other => panic!("an answer lands as Rsvp, got {other:?}"),
    };
    let outcome = |response: &str, send: bool, delivered: bool| {
        json!({
            "account": ACCOUNT, "event": "ev-meeting", "response": response,
            "comment": null, "send_response": send, "via": "server", "delivered": delivered,
        })
    };
    assert_eq!(
        landed(outcome("tentative", false, false)),
        Ok("Tentatively accepted; no response sent to the organizer".to_string())
    );
    assert_eq!(
        landed(outcome("accept", true, true)),
        Ok("Accepted; the organizer was told".to_string())
    );
    assert_eq!(
        landed(outcome("decline", true, false)),
        Err("Declined, but the organizer could not be told".to_string())
    );
}
