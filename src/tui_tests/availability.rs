//! The event form's availability rows (PERSO-255, phase 2, slice 7): the
//! frame of the rows under the form, and the query they follow, asked once
//! it has stood still, its answer landing only while it is still the one
//! asked.
//!
//! The query itself is `calendar.freebusy`, which
//! `tests/daemon_freebusy_slice.rs` drives against a daemon process; here the
//! TUI's call stops at a scripted door, and its settle is the payload the
//! daemon's `operation.finished` carries.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use insta::assert_snapshot;
use serde_json::{json, Value};

use mp_protocol::calendar::{
    CalendarFreeBusyResult, CalendarInfo, CalendarSchedule, CalendarScheduleItem,
};

use crate::tui::app::{
    Action, App, Availability, EventCalendarChoice, EventForm, Overlay, AVAILABILITY_DEBOUNCE,
};
use crate::tui::commands::{dispatch, settled};
use crate::tui::events::Awaited;
use crate::tui::queries::Queries;
use crate::tui::ui::golden_frames::{calendar_fixture, frame_snapshot, HEIGHT, WIDTH};

/// The event form on Tuesday 4 August 2026, 14:00 to 15:30 in Berlin, with
/// an attendee, an outside address and a room.
fn form() -> EventForm {
    let calendars = vec![EventCalendarChoice {
        account: "evoqs".into(),
        id: "cal-main".into(),
        name: "Kalender".into(),
        teams: true,
    }];
    let day = chrono::NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();
    let mut form = EventForm::new(calendars, "evoqs", day, "Europe/Berlin");
    form.title = "Budget review".into();
    form.start = "14:00".into();
    form.end = "15:30".into();
    form.to = "ada@example.org, someone@gmail.com".into();
    form.rooms = "room-1@example.org".into();
    form
}

/// `calendar.list`'s rows of the account, with `free-busy` or without.
fn calendars(free_busy: bool) -> Vec<CalendarInfo> {
    let mut features = vec!["write".to_string(), "online-meeting".to_string()];
    if free_busy {
        features.push("free-busy".into());
    }
    vec![CalendarInfo {
        account: "evoqs".into(),
        id: "cal-main".into(),
        name: "Kalender".into(),
        color: None,
        is_default: true,
        can_edit: true,
        visible: true,
        backend: "graph".into(),
        features,
    }]
}

/// A view of the Berlin day from `2026-08-03T22:00Z` at 30-minute slots,
/// free but for `marks` (slot, digit).
fn view(marks: &[(usize, char)]) -> String {
    let mut slots = vec!['0'; 48];
    for (slot, digit) in marks {
        slots[*slot] = *digit;
    }
    slots.into_iter().collect()
}

/// The settled answer for the form: the user busy 09:00 to 10:00, Ada away
/// 13:00 to 14:00 and tentative 15:00 to 15:30, the room free, the outside
/// address without free/busy.
fn answer() -> CalendarFreeBusyResult {
    let schedule = |address: &str, view: String| CalendarSchedule {
        address: address.into(),
        view,
        items: Vec::new(),
        error: None,
    };
    let mut own = schedule("sylvain@example.org", view(&[(18, '2'), (19, '2')]));
    own.items.push(CalendarScheduleItem {
        start: "2026-08-04T07:00:00Z".into(),
        end: "2026-08-04T08:00:00Z".into(),
        status: "busy".into(),
        subject: Some("Jour fixe".into()),
        location: None,
    });
    CalendarFreeBusyResult {
        account: "evoqs".into(),
        start: "2026-08-03T22:00:00Z".into(),
        end: "2026-08-04T22:00:00Z".into(),
        interval_minutes: 30,
        schedules: vec![
            own,
            schedule("ada@example.org", view(&[(26, '3'), (27, '3'), (30, '1')])),
            CalendarSchedule {
                address: "someone@gmail.com".into(),
                view: String::new(),
                items: Vec::new(),
                error: Some("Proxy web request failed (5016)".into()),
            },
            schedule("room-1@example.org", view(&[])),
        ],
    }
}

/// The form with its availability rows: a row per schedule from 07:00 to
/// 20:00 under the hour ruler, the outside address reading "no free/busy";
/// and the rows while the first query is still on its way.
#[test]
fn golden_calendar_event_form_availability() {
    let mut app = calendar_fixture();
    let form = form();
    let key = form.free_busy_key().unwrap();
    app.overlay = Overlay::EventForm(Box::new(form));
    app.calendar_view.availability = Availability {
        seen: Some((key.clone(), Instant::now())),
        asked: Some(key.clone()),
        operation: None,
        answer: Some(Ok(answer())),
    };
    assert_snapshot!(
        "event_form_availability",
        frame_snapshot(&mut app, WIDTH, HEIGHT)
    );

    app.calendar_view.availability = Availability {
        seen: Some((key.clone(), Instant::now())),
        asked: Some(key),
        operation: Some("op-1".into()),
        answer: None,
    };
    assert_snapshot!(
        "event_form_availability_checking",
        frame_snapshot(&mut app, WIDTH, HEIGHT)
    );
}

/// The rows' glyphs, row by row, the chosen time's slots among them.
#[test]
fn the_rows_read_the_days_slots_from_seven_to_eight() {
    let mut app = calendar_fixture();
    let form = form();
    let key = form.free_busy_key().unwrap();
    app.overlay = Overlay::EventForm(Box::new(form));
    app.calendar_view.availability = Availability {
        seen: Some((key.clone(), Instant::now())),
        asked: Some(key),
        operation: None,
        answer: Some(Ok(answer())),
    };
    let frame = frame_snapshot(&mut app, WIDTH, HEIGHT);
    let row = |address: &str| -> String {
        let line = frame
            .lines()
            .find(|line| {
                line.contains(address) && !line.contains("Attendees") && !line.contains("Rooms")
            })
            .unwrap_or_else(|| panic!("no row for {address} in\n{frame}"));
        let at = line.find(address).unwrap() + 23;
        line.chars()
            .skip(line[..at].chars().count())
            .take(26)
            .collect()
    };
    let dots = |n: usize| "\u{b7}".repeat(n);
    assert_eq!(
        row("sylvain@example.org"),
        format!("{}\u{2588}\u{2588}{}", dots(4), dots(20))
    );
    assert_eq!(
        row("ada@example.org"),
        format!("{}\u{2593}\u{2593}{}\u{2592}{}", dots(12), dots(2), dots(9))
    );
    assert_eq!(row("room-1@example.org"), dots(26));
    assert!(frame
        .lines()
        .any(|line| line.contains("someone@gmail.com") && line.contains("no free/busy")));
    assert!(frame.contains("07  09  11  13  15  17  19"), "{frame}");
}

/// Answers `calendar.freebusy` with a fresh operation id each call, or
/// refuses once `refuse` is set.
#[derive(Default)]
struct Door {
    calls: Mutex<Vec<Value>>,
    refuse: Mutex<Option<String>>,
}

impl Queries for Door {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        assert_eq!(method, "calendar.freebusy");
        if let Some(why) = self.refuse.lock().unwrap().clone() {
            anyhow::bail!("{why}");
        }
        let mut calls = self.calls.lock().unwrap();
        calls.push(params);
        Ok(json!({"operation_id": format!("op-fb-{}", calls.len())}))
    }
}

/// An app with the form open on an account whose calendars do or do not
/// serve free/busy.
fn app_with_form(free_busy: bool) -> App {
    let mut app = App::default_for_tests();
    app.calendar_view.calendars = calendars(free_busy);
    app.overlay = Overlay::EventForm(Box::new(form()));
    app
}

/// The `QueryFreeBusy` actions queued so far, taken off the queue.
fn queries(app: &mut App) -> Vec<mp_protocol::calendar::CalendarFreeBusyParams> {
    let mut asked = Vec::new();
    app.pending_actions.retain(|action| match action {
        Action::QueryFreeBusy(params) => {
            asked.push((**params).clone());
            false
        }
        _ => true,
    });
    asked
}

/// The settle of `operation_id` with `result`, as the event drain lands it.
fn land(app: &mut App, operation_id: &str, result: &CalendarFreeBusyResult) {
    let landed = settled(
        &Awaited::FreeBusy {
            operation_id: operation_id.into(),
        },
        &json!({"operation_id": operation_id, "state": "succeeded",
                "result": serde_json::to_value(result).unwrap()}),
    );
    crate::tui::bg::handle_bg_result(app, landed);
}

/// The form's query is asked once it has stood still for the debounce, not
/// per keystroke; a changed address asks again; an answer for the query no
/// longer asked is dropped; a closed form forgets its rows.
#[test]
fn the_form_asks_free_busy_once_its_query_stands_still() {
    let mut app = app_with_form(true);
    let door = Door::default();
    let t0 = Instant::now();
    app.tick_availability(t0);
    app.tick_availability(t0 + Duration::from_millis(100));
    assert!(queries(&mut app).is_empty(), "not before the debounce");
    assert!(app.tick_availability(t0 + AVAILABILITY_DEBOUNCE));
    let asked = queries(&mut app);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].account, "evoqs");
    assert_eq!(
        asked[0].addresses,
        ["ada@example.org", "someone@gmail.com", "room-1@example.org"]
    );
    assert_eq!(
        (asked[0].start.as_str(), asked[0].end.as_str()),
        ("2026-08-04", "2026-08-04")
    );
    assert_eq!(asked[0].zone.as_deref(), Some("Europe/Berlin"));
    assert!(dispatch(
        &mut app,
        &door,
        &Action::QueryFreeBusy(Box::new(asked[0].clone()))
    ));
    assert_eq!(
        app.calendar_view.availability.operation.as_deref(),
        Some("op-fb-1")
    );
    assert_eq!(app.bg_count, 1);
    app.tick_availability(t0 + Duration::from_secs(2));
    assert!(queries(&mut app).is_empty(), "the same query is asked once");

    // Typing a new address: each change restarts the wait, and a
    // half-typed address is not asked about.
    let t1 = t0 + Duration::from_secs(3);
    let type_to = |app: &mut App, to: &str| {
        if let Overlay::EventForm(form) = &mut app.overlay {
            form.to = to.into();
        }
    };
    type_to(&mut app, "ada@example.org, kim@exa");
    app.tick_availability(t1);
    type_to(&mut app, "ada@example.org, kim@example.org");
    app.tick_availability(t1 + Duration::from_millis(100));
    app.tick_availability(t1 + Duration::from_millis(400));
    assert!(
        queries(&mut app).is_empty(),
        "the debounce runs from the last change"
    );
    app.tick_availability(t1 + Duration::from_millis(100) + AVAILABILITY_DEBOUNCE);
    let asked = queries(&mut app);
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].addresses,
        ["ada@example.org", "kim@example.org", "room-1@example.org"]
    );
    assert!(dispatch(
        &mut app,
        &door,
        &Action::QueryFreeBusy(Box::new(asked[0].clone()))
    ));

    land(&mut app, "op-fb-1", &answer());
    assert!(
        app.calendar_view.availability.answer.is_none(),
        "the first query's answer is stale"
    );
    land(&mut app, "op-fb-2", &answer());
    assert_eq!(app.calendar_view.availability.answer, Some(Ok(answer())));
    assert!(!app.calendar_view.availability.checking());
    assert_eq!(app.bg_count, 0);

    app.overlay = Overlay::None;
    assert!(app.tick_availability(t1 + Duration::from_secs(5)));
    assert!(app.calendar_view.availability.answer.is_none());
    assert!(app.calendar_view.availability.seen.is_none());
}

/// An account whose calendars lack `free-busy` asks nothing and shows no
/// rows; a refused query is the rows' sentence.
#[test]
fn no_free_busy_asks_nothing_and_a_refusal_is_the_rows_sentence() {
    let mut app = app_with_form(false);
    let t0 = Instant::now();
    app.tick_availability(t0);
    app.tick_availability(t0 + Duration::from_secs(1));
    assert!(queries(&mut app).is_empty());
    assert!(app.calendar_view.availability.seen.is_none());

    let mut app = app_with_form(true);
    app.tick_availability(t0);
    app.tick_availability(t0 + Duration::from_secs(1));
    let asked = queries(&mut app);
    let door = Door::default();
    *door.refuse.lock().unwrap() = Some("calendar needs a re-login".into());
    assert!(dispatch(
        &mut app,
        &door,
        &Action::QueryFreeBusy(Box::new(asked[0].clone()))
    ));
    assert_eq!(
        app.calendar_view.availability.answer,
        Some(Err("calendar needs a re-login".into()))
    );
    assert_eq!(app.bg_count, 0, "nothing is awaited");
    let frame = frame_snapshot(&mut app, WIDTH, HEIGHT);
    assert!(
        frame.contains("unavailable: calendar needs a re-login"),
        "{frame}"
    );
}

/// The text rows of `frame_snapshot`'s output, without its style runs.
fn text_rows(frame: &str) -> Vec<&str> {
    frame
        .lines()
        .skip(1)
        .take_while(|line| !line.starts_with("== style runs"))
        .collect()
}

/// On a short terminal the form keeps its fields, its problem line and its
/// keys, and shows as many availability rows as the height leaves, the rest
/// counted: at 80x26 the header and one row, at 80x24 the count alone, at
/// 80x23 no availability at all.
#[test]
fn a_short_terminal_keeps_the_problem_line_under_the_rows() {
    let mut app = calendar_fixture();
    let mut form = form();
    form.end = "13:00".into();
    let key = form.free_busy_key().unwrap();
    app.overlay = Overlay::EventForm(Box::new(form));
    app.calendar_view.availability = Availability {
        seen: Some((key.clone(), Instant::now())),
        asked: Some(key),
        operation: None,
        answer: Some(Ok(answer())),
    };
    let frame = frame_snapshot(&mut app, 80, 24);
    assert_snapshot!("event_form_availability_80x24", frame);
    for (height, rows, more) in [(26, 1, "+3 more"), (24, 0, "+4 more"), (23, 0, "")] {
        let frame = frame_snapshot(&mut app, 80, height);
        let text = text_rows(&frame);
        let at = |needle: &str| text.iter().position(|line| line.contains(needle));
        let problem = at("The end comes before the start")
            .unwrap_or_else(|| panic!("no problem line at 80x{height}:\n{frame}"));
        let description = at("Description").expect("the last field");
        let keys = at("Enter create").expect("the keys");
        assert!(description < problem && problem < keys, "{frame}");
        let shown = text
            .iter()
            .filter(|line| line.contains("@example.org ") && !line.contains("Attendees"))
            .filter(|line| !line.contains("Rooms"))
            .count();
        assert_eq!(shown, rows, "rows at 80x{height}:\n{frame}");
        assert_eq!(more.is_empty(), !frame.contains("more"), "{frame}");
        if !more.is_empty() {
            assert!(frame.contains(more), "{more} at 80x{height}:\n{frame}");
        }
    }
    // Shorter still, the last fields go before the problem line does.
    let frame = frame_snapshot(&mut app, 80, 22);
    assert!(frame.contains("The end comes before the start"), "{frame}");
    assert!(frame.contains("Enter create"), "{frame}");
}

/// A refusal too long for the row is cut with an ellipsis.
#[test]
fn a_long_refusal_is_cut_with_an_ellipsis() {
    let mut app = calendar_fixture();
    let form = form();
    let key = form.free_busy_key().unwrap();
    app.overlay = Overlay::EventForm(Box::new(form));
    let why = "The free/busy query failed: the server answered a long sentence \
               that runs well past the form's right border";
    app.calendar_view.availability = Availability {
        seen: Some((key.clone(), Instant::now())),
        asked: Some(key),
        operation: None,
        answer: Some(Err(why.into())),
    };
    let frame = frame_snapshot(&mut app, WIDTH, HEIGHT);
    let line = text_rows(&frame)
        .into_iter()
        .find(|line| line.contains("unavailable: "))
        .unwrap_or_else(|| panic!("no refusal in\n{frame}"));
    assert!(
        line.contains("unavailable: The free/busy query failed: the\u{2026}"),
        "{line}"
    );
}
