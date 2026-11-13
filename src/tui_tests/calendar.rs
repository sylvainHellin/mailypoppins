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
    Attendee, AttendeeRole, Calendar, Event, EventKind, EventTime, OnlineMeetingProvider,
    OwnResponse, Person, ResponseStatus, Sensitivity, ShowAs,
};
use crate::calendar_sync::sync_account;
use crate::config::{AccountConfig, CalendarBackendKind};
use crate::tui::app::{
    AccountState, Action, App, BgResult, CalendarLayout, Credentials, EventCalendarChoice,
    EventForm, EventFormRow, EventRemoveOverlay, EventScopeChooser, Overlay, RsvpChoice,
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

/// The event form on the Tuesday, a meeting being typed: the calendar, the
/// times, two attendees and a room, Teams on, the cursor on the location;
/// and the same form back after a failed create, its error under the rows.
#[test]
fn golden_calendar_event_form() {
    let mut app = calendar_fixture();
    let calendars = vec![
        EventCalendarChoice {
            account: "evoqs".into(),
            id: "cal-main".into(),
            name: "Kalender".into(),
            teams: true,
        },
        EventCalendarChoice {
            account: "evoqs".into(),
            id: "cal-loc".into(),
            name: "LOC".into(),
            teams: true,
        },
    ];
    let day = chrono::NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();
    let mut form = EventForm::new(calendars, "evoqs", day, "Europe/Berlin");
    form.title = "Budget review".into();
    form.start = "14:00".into();
    form.end = "15:30".into();
    form.to = "ada@example.com, bob@example.com".into();
    form.rooms = "room-1@example.com".into();
    form.teams = true;
    form.body = "Agenda\n1. Numbers\n2. Next steps".into();
    form.row = EventFormRow::Location;
    form.location = "Room 1".into();
    app.overlay = Overlay::EventForm(Box::new(form.clone()));
    assert_snapshot!("event_form", frame_snapshot(&mut app, WIDTH, HEIGHT));

    form.transaction_id = Some("mp-1".into());
    form.error = Some(
        "The event was not created: Graph refused the new event: HTTP 403; Enter retries it".into(),
    );
    form.row = EventFormRow::Title;
    app.overlay = Overlay::EventForm(Box::new(form));
    assert_snapshot!("event_form_retry", frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// The respond overlay on a decline proposing a new time (PERSO-255): the
/// proposed start being typed, the end left to the meeting's length; and the
/// same overlay refusing the proposal with the response off, on its error
/// line.
#[test]
fn golden_calendar_respond_overlay_proposing() {
    let mut app = calendar_fixture();
    let mut overlay = RsvpOverlay::new(
        "evoqs".to_string(),
        "ev-jour-1".to_string(),
        "Jour fixe".to_string(),
    );
    overlay.selected = 2;
    overlay.row = RsvpRow::ProposeStart;
    overlay.proposed_start = "2026-08-04T14:00".to_string();
    overlay.zone = "Europe/Berlin".to_string();
    app.overlay = Overlay::Rsvp(overlay);
    assert_snapshot!("respond_propose", frame_snapshot(&mut app, WIDTH, HEIGHT));

    if let Overlay::Rsvp(overlay) = &mut app.overlay {
        overlay.send_response = false;
        overlay.row = RsvpRow::SendResponse;
        overlay.error = overlay.proposal().err();
    }
    assert_snapshot!(
        "respond_propose_refused",
        frame_snapshot(&mut app, WIDTH, HEIGHT)
    );
}

/// The fixture's handover as the user's own meeting, two attendees having
/// proposed other times.
fn proposed_handover(app: &App) -> mp_protocol::calendar::CalendarEvent {
    let mut handover = app.calendar_view.events[0].clone();
    assert_eq!(handover.id, "ev-handover");
    handover.is_organizer = true;
    handover.own_response = "organizer".into();
    handover.organizer = Some(mp_protocol::calendar::CalendarPerson {
        name: None,
        email: "sylvain@example.org".into(),
    });
    handover.attendees[0].email = "bob@example.org".into();
    handover.attendees[0].name = Some("Bob".into());
    handover.attendees[0].proposed_time = Some(mp_protocol::calendar::CalendarTimeSlot {
        start: "2026-08-04T12:00:00Z".into(),
        end: "2026-08-04T13:00:00Z".into(),
    });
    handover
}

/// The organizer's card of a meeting with two proposals: the one `p` takes
/// is marked, `Tab` moved to the second, and the keys name `p`.
#[test]
fn golden_calendar_event_card_with_proposals() {
    let mut app = calendar_fixture();
    let mut card = crate::tui::app::EventCardOverlay::new(proposed_handover(&app));
    card.proposal = 1;
    app.overlay = Overlay::EventCard(Box::new(card));
    assert_snapshot!(
        "event_card_proposals",
        frame_snapshot(&mut app, WIDTH, HEIGHT)
    );
}

/// `p` on the organizer's card opens the edit form at the chosen proposal,
/// `Tab` choosing the next, so `Enter` sends the new time as `event.update`;
/// on an occurrence of a series the form is the occurrence's alone, its
/// update in the occurrence scope with no chooser, and on a series master
/// the update names no scope, which the daemon reads as the series'; an
/// attendee's card and a meeting nobody proposed for are refused on the
/// status line.
#[test]
fn p_on_the_card_opens_the_edit_form_at_the_proposed_time() {
    let mut app = calendar_fixture();
    for calendar in &mut app.calendar_view.calendars {
        calendar.features = vec!["write".into()];
    }
    let handover = proposed_handover(&app);
    app.overlay = Overlay::EventCard(Box::new(crate::tui::app::EventCardOverlay::new(
        handover.clone(),
    )));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.handle_key(key('p'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("p opens the form: {:?}", app.status_message);
    };
    assert_eq!(
        (form.date.as_str(), form.start.as_str(), form.end.as_str()),
        ("2026-08-04", "14:00", "15:00"),
        "two Tabs come back to the first proposal, Bob's, in the view's zone"
    );
    assert_eq!(form.row, EventFormRow::Start);
    assert!(app
        .status_message
        .as_deref()
        .is_some_and(|m| m.starts_with("The form holds Bob's proposed time")));
    let fields = form.changed_fields();
    assert_eq!(fields.start.as_deref(), Some("2026-08-04T14:00"));
    assert_eq!(fields.end.as_deref(), Some("2026-08-04T15:00"));
    assert_eq!(fields.summary, None);

    app.overlay = Overlay::EventCard(Box::new(crate::tui::app::EventCardOverlay::new(
        handover.clone(),
    )));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.handle_key(key('p'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("p opens the form");
    };
    assert_eq!(
        (form.date.as_str(), form.start.as_str()),
        ("2026-08-03", "11:00")
    );

    let refused = |app: &mut App, event: mp_protocol::calendar::CalendarEvent| {
        app.overlay = Overlay::EventCard(Box::new(crate::tui::app::EventCardOverlay::new(event)));
        app.handle_key(key('p'));
        assert!(
            matches!(app.overlay, Overlay::EventCard(_)),
            "the card stays"
        );
        app.status_message.clone().unwrap_or_default()
    };
    let mut theirs = handover.clone();
    theirs.is_organizer = false;
    let line = refused(&mut app, theirs);
    assert!(
        line.starts_with("Only the organizer takes a proposed time"),
        "{line}"
    );
    let mut occurrence = handover.clone();
    occurrence.kind = "occurrence".into();
    occurrence.series_master_id = Some("ev-series".into());
    occurrence.recurrence = Some("Weekly on Monday".into());
    app.overlay = Overlay::EventCard(Box::new(crate::tui::app::EventCardOverlay::new(
        occurrence.clone(),
    )));
    app.handle_key(key('p'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!(
            "p on an occurrence opens its form, no chooser: {:?}",
            app.status_message
        );
    };
    assert_eq!(form.scope(), Some("occurrence"));
    assert_eq!(
        (form.date.as_str(), form.start.as_str(), form.end.as_str()),
        ("2026-08-04", "14:00", "15:00")
    );
    let params = form.update_params().expect("the occurrence's update");
    assert_eq!(params.event, occurrence.id);
    assert_eq!(params.scope.as_deref(), Some("occurrence"));
    assert_eq!(params.fields.start.as_deref(), Some("2026-08-04T14:00"));
    assert_eq!(params.fields.end.as_deref(), Some("2026-08-04T15:00"));
    assert_eq!(params.fields.recurrence, None);

    let mut master = handover.clone();
    master.kind = "series-master".into();
    master.recurrence = Some("Weekly on Monday".into());
    app.overlay = Overlay::EventCard(Box::new(crate::tui::app::EventCardOverlay::new(master)));
    app.handle_key(key('p'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("p on a series master opens its form");
    };
    let params = form.update_params().expect("the series' update");
    assert_eq!(
        params.scope, None,
        "the daemon reads a master's change as the series'"
    );
    assert_eq!(params.fields.start.as_deref(), Some("2026-08-04T14:00"));

    let mut nobody = handover;
    for attendee in &mut nobody.attendees {
        attendee.proposed_time = None;
    }
    let line = refused(&mut app, nobody);
    assert!(line.starts_with("Nobody proposed a new time"), "{line}");
}

/// The card of the fixture's `index`th row, opened the way a user opens it:
/// the cursor on the row and `Enter` in the view.
fn card_on(app: &mut App, index: usize, summary: &str) {
    app.calendar_view.list_index = index;
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Overlay::EventCard(card) = &app.overlay else {
        panic!("Enter opens the card of {summary}");
    };
    assert_eq!(card.event.summary, summary);
}

/// `V` on the card closes it and opens the respond overlay against the
/// card's own event; on an event there is nothing to answer, the card closes
/// and the status line says why.
#[test]
fn v_on_the_card_opens_the_respond_overlay_for_its_event() {
    let mut app = calendar_fixture();
    card_on(&mut app, 1, "Jour fixe");
    app.handle_key(key('V'));
    let Overlay::Rsvp(overlay) = &app.overlay else {
        panic!(
            "V on the card opens the respond overlay: {:?}",
            app.status_message
        );
    };
    assert_eq!(overlay.account, "evoqs");
    assert_eq!(overlay.event, "ev-jour-1");
    assert_eq!(overlay.summary, "Jour fixe");

    let mut app = calendar_fixture();
    card_on(&mut app, 6, "Abgesagter Termin");
    app.handle_key(key('V'));
    assert!(matches!(app.overlay, Overlay::None));
    assert!(app
        .status_message
        .as_deref()
        .is_some_and(|m| m.contains("cancelled")));
}

/// `E` on the card closes it and opens the event form filled with the
/// card's event; on an invitation mail's event and on a calendar mp does not
/// write to, the card closes and the status line says why.
#[test]
fn shift_e_on_the_card_opens_the_event_form_with_its_values() {
    let mut app = calendar_fixture();
    for calendar in &mut app.calendar_view.calendars {
        calendar.features = vec!["write".into()];
    }
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    app.handle_key(key('E'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("E on the card opens the form: {:?}", app.status_message);
    };
    let editing = form.editing.as_ref().expect("the form edits the event");
    assert_eq!(
        (editing.account.as_str(), editing.event.as_str()),
        ("evoqs", "ev-handover")
    );
    assert_eq!(form.title, "Baustellen\u{fc}bergabe");
    assert_eq!(
        (form.date.as_str(), form.start.as_str(), form.end.as_str()),
        ("2026-08-03", "09:00", "10:00"),
        "the event's times in the view's zone"
    );
    assert_eq!(form.location, "Raum 2.14");

    app.overlay = Overlay::None;
    card_on(&mut app, 2, "PhD Kolloquium");
    app.handle_key(key('E'));
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(
        app.status_message.as_deref(),
        Some("An invitation mail's event can only be answered (V)")
    );

    let mut app = calendar_fixture();
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    app.handle_key(key('E'));
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(
        app.status_message.as_deref(),
        Some("The calendar Calendar takes no changes from mp")
    );
}

/// `D` on the card closes it and opens the cancel confirmation of a meeting
/// the user organizes, naming its attendees; on someone else's meeting the
/// card closes and the status line points at the answer instead.
#[test]
fn d_on_the_card_opens_the_cancel_confirmation_or_refuses_a_foreign_meeting() {
    let mut app = calendar_fixture();
    for calendar in &mut app.calendar_view.calendars {
        calendar.features = vec!["write".into()];
    }
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    app.handle_key(key('D'));
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(
        app.status_message.as_deref(),
        Some(
            "Baustellen\u{fc}bergabe is someone else's meeting: decline it with V to tell the \
             organizer"
        )
    );

    app.calendar_view.events[0].is_organizer = true;
    app.calendar_view.events[0].own_response = "organizer".into();
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    app.handle_key(key('D'));
    let Overlay::EventRemove(removal) = &app.overlay else {
        panic!("D on the card asks to cancel: {:?}", app.status_message);
    };
    assert_eq!(
        (removal.account.as_str(), removal.event.as_str()),
        ("evoqs", "ev-handover")
    );
    assert!(removal.cancel, "the organizer's meeting is cancelled");
    assert_eq!(removal.attendees, 2);
    assert_eq!(removal.scope, None);
}

/// An invitation mail the account organised is changed and cancelled by
/// mail (PERSO-204): `E` opens the form on it and `D` asks to cancel it,
/// telling its attendees, both through the daemon's `event.update` and
/// `event.cancel`; a repeating one is cancelled rather than changed, and one
/// cancelled already says so.
#[test]
fn an_invitation_the_account_organised_takes_e_and_d() {
    let organised = |app: &mut App| {
        let event = app
            .calendar_view
            .events
            .iter_mut()
            .find(|e| e.id == "imip:3")
            .expect("the fixture's invitation mail");
        event.is_organizer = true;
        event.own_response = "organizer".into();
        event.organizer = Some(mp_protocol::calendar::CalendarPerson {
            name: None,
            email: "perso@example.org".into(),
        });
        event.attendees = vec![mp_protocol::calendar::CalendarAttendee {
            name: None,
            email: "ada@example.org".into(),
            role: "required".into(),
            status: "needs-action".into(),
            proposed_time: None,
        }];
    };
    let mut app = calendar_fixture();
    organised(&mut app);
    card_on(&mut app, 2, "PhD Kolloquium");
    app.handle_key(key('E'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("E opens the form: {:?}", app.status_message);
    };
    let editing = form.editing.as_ref().expect("the form edits the event");
    assert_eq!(
        (editing.account.as_str(), editing.event.as_str()),
        ("perso", "imip:3")
    );
    assert!(editing.is_organizer);

    app.overlay = Overlay::None;
    card_on(&mut app, 2, "PhD Kolloquium");
    app.handle_key(key('D'));
    let Overlay::EventRemove(removal) = &app.overlay else {
        panic!("D asks to cancel: {:?}", app.status_message);
    };
    assert_eq!(removal.event, "imip:3");
    assert!(removal.cancel, "the attendees are told");
    assert_eq!(removal.attendees, 1);

    let mut app = calendar_fixture();
    organised(&mut app);
    for event in app
        .calendar_view
        .events
        .iter_mut()
        .filter(|e| e.id == "imip:3")
    {
        event.kind = "series-master".into();
    }
    card_on(&mut app, 2, "PhD Kolloquium");
    app.handle_key(key('E'));
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(
        app.status_message.as_deref(),
        Some("mp cannot change a repeating invitation yet; D cancels PhD Kolloquium")
    );

    let mut app = calendar_fixture();
    organised(&mut app);
    for event in app
        .calendar_view
        .events
        .iter_mut()
        .filter(|e| e.id == "imip:3")
    {
        event.cancelled = true;
    }
    for k in ['D', 'E'] {
        card_on(&mut app, 2, "PhD Kolloquium");
        app.handle_key(key(k));
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(
            app.status_message.as_deref(),
            Some("PhD Kolloquium is cancelled already; an invitation mail stays on the agenda"),
            "{k}"
        );
    }
}

/// `e` on the card of an invitation mail's event closes it and queues the
/// invitation's `.ics` for `$EDITOR`, the CALENDAR view's `e`; on a server
/// event there is no mail behind it, so the card closes, nothing is queued
/// and the status line says so.
#[test]
fn e_on_the_card_opens_the_invitations_ics_only_for_a_mail() {
    let mut app = calendar_fixture();
    app.account_config.name = "perso".to_string();
    card_on(&mut app, 2, "PhD Kolloquium");
    app.handle_key(key('e'));
    assert!(matches!(app.overlay, Overlay::None));
    match app.pending_actions.pop_front() {
        Some(Action::OpenEventSource { msg }) => {
            assert_eq!(msg, crate::tui::app::MessageRef::new(3));
        }
        other => panic!("expected OpenEventSource, got {other:?}"),
    }
    assert!(app.pending_actions.is_empty());

    let mut app = calendar_fixture();
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    app.handle_key(key('e'));
    assert!(matches!(app.overlay, Overlay::None));
    assert!(app.pending_actions.is_empty(), "a server event has no mail");
    assert_eq!(app.view, View::Calendar);
    assert_eq!(
        app.status_message.as_deref(),
        Some("This event is the server's; no invitation mail stands behind it")
    );
}

/// `j`/`Down` and `k`/`Up` scroll a card taller than its area by a line:
/// the last lines come into view and go again, and `k` stops at the top;
/// `j` stops with the last line on the area's bottom row, so the first `k`
/// after pressing past it moves the view at once; a card that fits does
/// not scroll.
#[test]
fn j_and_k_scroll_the_card() {
    let mut app = calendar_fixture();
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    let scroll = |app: &App| match &app.overlay {
        Overlay::EventCard(card) => card.scroll,
        _ => panic!("the card stays open"),
    };
    // Fourteen lines in a frame of twenty rows: the body is off the card.
    let shows_the_body =
        |app: &mut App| frame_snapshot(app, WIDTH, 20).contains("Bitte Helm mitbringen.");
    assert!(!shows_the_body(&mut app));
    app.handle_key(key('k'));
    assert_eq!(scroll(&app), 0, "k stops at the top");
    app.handle_key(key('j'));
    app.handle_key(KeyEvent::from(KeyCode::Down));
    assert_eq!(scroll(&app), 2);
    assert!(shows_the_body(&mut app));
    app.handle_key(key('k'));
    app.handle_key(KeyEvent::from(KeyCode::Up));
    assert_eq!(scroll(&app), 0);
    assert!(!shows_the_body(&mut app));

    // The frame's rows holding `text`, by index.
    let row_of = |frame: &str, text: &str| {
        frame
            .lines()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("no row holds {text:?}:\n{frame}"))
    };
    for _ in 0..30 {
        app.handle_key(key('j'));
    }
    let frame = frame_snapshot(&mut app, WIDTH, 20);
    let last = row_of(&frame, "Bitte Helm mitbringen.");
    let footer = row_of(&frame, "j/k scroll");
    assert_eq!(
        last + 1,
        footer,
        "the last line sits on the bottom row:\n{frame}"
    );
    let link = row_of(&frame, "Link ");
    let bottom = scroll(&app);
    app.handle_key(key('k'));
    assert_eq!(scroll(&app), bottom - 1, "one k moves the view up by one");
    let frame = frame_snapshot(&mut app, WIDTH, 20);
    assert!(!frame.contains("Bitte Helm mitbringen."), "{frame}");
    assert_eq!(row_of(&frame, "Link "), link + 1, "{frame}");

    // A frame the whole card fits in does not scroll at all.
    app.handle_key(key('j'));
    frame_snapshot(&mut app, WIDTH, HEIGHT);
    assert_eq!(scroll(&app), 0, "a card that fits does not scroll");
}

/// `q` and `Enter` close the card, as `Esc` does, and leave the view
/// standing: `q` here is the card's, not the quit key.
#[test]
fn q_and_enter_close_the_card() {
    let mut app = calendar_fixture();
    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    assert!(app.handle_key(key('q')).is_none(), "q does not quit");
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.view, View::Calendar);

    card_on(&mut app, 0, "Baustellen\u{fc}bergabe");
    assert!(app.handle_key(KeyEvent::from(KeyCode::Enter)).is_none());
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.view, View::Calendar);
    assert!(app.pending_actions.is_empty());
}

/// `V` offers a proposed time on a server event whose organizer accepts
/// proposals on a calendar that offers them, and on no other; the overlay
/// reads its times in the view's zone.
#[test]
fn v_offers_a_proposed_time_where_the_organizer_and_the_calendar_take_one() {
    let mut app = calendar_fixture();
    let jour = app.calendar_view.events[1].clone();
    assert_eq!(jour.id, "ev-jour-1");
    let propose = |app: &mut App, event: &mp_protocol::calendar::CalendarEvent| {
        app.overlay = Overlay::None;
        app.open_rsvp_overlay_for(event);
        match &app.overlay {
            Overlay::Rsvp(overlay) => (overlay.propose, overlay.zone.clone()),
            _ => panic!("V opens the overlay: {:?}", app.status_message),
        }
    };
    for calendar in &mut app.calendar_view.calendars {
        calendar.features = vec!["propose-time".into()];
    }
    assert_eq!(
        propose(&mut app, &jour),
        (true, "Europe/Berlin".to_string())
    );
    let mut fixed = jour.clone();
    fixed.allow_new_time_proposals = false;
    assert!(!propose(&mut app, &fixed).0);
    for calendar in &mut app.calendar_view.calendars {
        calendar.features.clear();
    }
    assert!(
        !propose(&mut app, &jour).0,
        "the backend offers no proposals"
    );
    let colloquium = app
        .calendar_view
        .events
        .iter()
        .find(|e| e.id == "imip:3")
        .cloned()
        .unwrap();
    assert!(
        !propose(&mut app, &colloquium).0,
        "an invitation mail's event"
    );
}

/// A comment longer than its row shows its tail while it is typed, after an
/// ellipsis, so the last characters and the caret stay in view.
#[test]
fn a_long_comment_being_typed_shows_its_tail_and_caret() {
    let mut app = calendar_fixture();
    let mut overlay = RsvpOverlay::new(
        "evoqs".to_string(),
        "ev-jour-1".to_string(),
        "Jour fixe".to_string(),
    );
    overlay.row = RsvpRow::Comment;
    overlay.comment = format!("{}the end", "x".repeat(80));
    app.overlay = Overlay::Rsvp(overlay);
    let frame = frame_snapshot(&mut app, WIDTH, HEIGHT);
    let row = frame
        .lines()
        .find(|line| line.contains("Comment: "))
        .expect("the comment row");
    assert!(row.contains("Comment: \u{2026}x"), "{row}");
    assert!(row.contains("the end\u{258f}"), "{row}");
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
        online_meeting_providers: vec![OnlineMeetingProvider::TeamsForBusiness],
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
        proposed_time: None,
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

/// The re-login banner reads `account.credentials` again on the
/// `calendar.changed` a sign-in's sync publishes, so it leaves once the
/// calendar scope is granted instead of holding the startup answer.
#[test]
fn a_calendar_changed_event_asks_the_credentials_again() {
    let _data = seeded();
    let mut app = app_over(CallLog::default());
    app.apply_bootstrap(&mp_protocol::state::Bootstrap {
        instance_id: "tui-test".to_string(),
        revision: 1,
        ..Default::default()
    });
    let mut evoqs = AccountState::for_tests(ACCOUNT);
    evoqs.credentials = Some(Credentials {
        calendar_scope_missing: true,
        ..Credentials::default()
    });
    app.accounts = vec![evoqs];
    assert_eq!(app.calendar_scope_missing(), [ACCOUNT]);

    let applied = app.apply_event(&mp_protocol::EventEnvelope {
        instance_id: "tui-test".to_string(),
        revision: 2,
        kind: mp_protocol::events::KIND_CALENDAR_CHANGED.to_string(),
        payload: json!({"account": ACCOUNT}),
    });
    assert_eq!(applied, Applied::Ignored);
    assert!(
        app.calendar_scope_missing().is_empty(),
        "the daemon reports no missing scope"
    );
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
            send_response: true,
            proposed: None,
        },
    ));
    let made = door.0.lock().unwrap().clone();
    assert_eq!(
        made,
        [(
            "event.respond".to_string(),
            json!({
                "account": ACCOUNT, "event": "ev-meeting", "response": "tentative",
                "comment": "running late", "send_response": true,
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

/// `n` opens the event form on the account's default calendar, which the
/// daemon's `calendar.list` says writes; `Enter` with no title names the
/// problem and sends nothing; a typed form is one `event.create` in the
/// view's zone. A refusal and a failed create bring the form back as typed,
/// the second with the transaction id the daemon answered, which the retry
/// sends again.
#[test]
fn the_event_form_creates_through_event_create_and_retries_with_its_transaction_id() {
    let _data = seeded();
    let mut app = app_over(CallLog::default());
    app.switch_view(View::Calendar);
    app.handle_key(key('n'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("n opens the event form");
    };
    assert_eq!(form.chosen().label(), "evoqs/Calendar");
    assert_eq!(form.row, EventFormRow::Title);
    assert_eq!(
        form.date,
        at(2, 0).date_naive().to_string(),
        "the cursor's day"
    );

    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("a form with no title stays open");
    };
    assert_eq!(form.error.as_deref(), Some("The event needs a title"));
    for c in "Planning".chars() {
        app.handle_key(key(c));
    }
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(app.overlay, Overlay::None));
    let Some(Action::CreateEvent(form)) = app.pending_actions.pop_front() else {
        panic!("Enter queues the create");
    };

    /// Refuses the first call, then starts an operation.
    #[derive(Default)]
    struct Door(Mutex<Vec<Value>>);
    impl Queries for Door {
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            assert_eq!(method, "event.create");
            let mut calls = self.0.lock().unwrap();
            calls.push(params);
            if calls.len() == 1 {
                anyhow::bail!("The calendar Calendar of evoqs is read-only");
            }
            Ok(json!({"operation_id": format!("op-{}", calls.len()), "transaction_id": "mp-tx"}))
        }
    }
    let door = Door::default();
    assert!(dispatch(&mut app, &door, &Action::CreateEvent(form)));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("a refusal reopens the form");
    };
    assert_eq!(form.title, "Planning");
    assert!(form.error.as_deref().unwrap().contains("read-only"));
    assert_eq!(form.transaction_id, None);

    let form = form.clone();
    app.overlay = Overlay::None;
    assert!(dispatch(&mut app, &door, &Action::CreateEvent(form)));
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.bg_count, 1, "the create is background work");
    let sent = door.0.lock().unwrap()[1].clone();
    assert_eq!(sent["account"], ACCOUNT);
    assert_eq!(sent["calendar"], "cal-main");
    assert_eq!(sent["event"]["summary"], "Planning");
    assert_eq!(sent["event"]["zone"], "UTC");
    assert!(sent.get("transaction_id").is_none(), "{sent}");

    let failed = settled(
        &Awaited::EventCreate {
            operation_id: "op-2".into(),
        },
        &json!({"operation_id": "op-2", "state": "failed",
                "error": {"code": -32603, "message": "Graph refused the new event: HTTP 503"}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, failed);
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("a failed create reopens the form");
    };
    assert_eq!(form.transaction_id.as_deref(), Some("mp-tx"));
    assert!(form.error.as_deref().unwrap().contains("HTTP 503"));
    let form = form.clone();
    app.overlay = Overlay::None;
    assert!(dispatch(&mut app, &door, &Action::CreateEvent(form)));
    assert_eq!(door.0.lock().unwrap()[2]["transaction_id"], "mp-tx");

    let created = settled(
        &Awaited::EventCreate {
            operation_id: "op-3".into(),
        },
        &json!({"operation_id": "op-3", "state": "succeeded", "result": {
            "account": ACCOUNT, "transaction_id": "mp-tx", "invited": ["ada@example.com"],
            "event": serde_json::to_value(crate::calendar_view::Sourced {
                event: server_event("ev-new", "cal-main", "Planning",
                    EventTime::timed(at(0, 9), at(0, 10), "UTC").unwrap()),
                source: None,
            }.to_wire().unwrap()).unwrap(),
        }}),
    );
    crate::tui::bg::handle_bg_result(&mut app, created);
    assert!(app.calendar_view.pending_forms.is_empty());
    assert_eq!(
        app.status_message.as_deref(),
        Some("Created Planning; invited ada@example.com")
    );
}

// ---------------------------------------------------------------------------
// Changing, cancelling and deleting an event (PERSO-255)
// ---------------------------------------------------------------------------

/// The edit form on a meeting the user organizes, its start moved; and the
/// `D` confirmations, a cancel with its comment being typed, the same cancel
/// asking again after its daemon restarted, and a delete of an event that
/// tells nobody.
#[test]
fn golden_calendar_event_edit_and_remove() {
    let mut app = calendar_fixture();
    let mut handover = app.calendar_view.events[0].clone();
    handover.is_organizer = true;
    handover.own_response = "organizer".into();
    let choice = EventCalendarChoice {
        account: "evoqs".into(),
        id: "cal-main".into(),
        name: "Calendar".into(),
        teams: true,
    };
    let mut form = EventForm::edit(&handover, choice, app.calendar_view.zone);
    form.start = "10:00".into();
    form.end = "11:00".into();
    form.row = EventFormRow::End;
    app.overlay = Overlay::EventForm(Box::new(form));
    assert_snapshot!("event_form_edit", frame_snapshot(&mut app, WIDTH, HEIGHT));

    app.overlay = Overlay::EventRemove(Box::new(EventRemoveOverlay {
        account: "evoqs".into(),
        event: "ev-handover".into(),
        summary: "Baustellen\u{fc}bergabe".into(),
        cancel: true,
        attendees: 2,
        comment: "Verschoben".into(),
        error: None,
        scope: None,
    }));
    assert_snapshot!("event_cancel", frame_snapshot(&mut app, WIDTH, HEIGHT));

    if let Overlay::EventRemove(removal) = &mut app.overlay {
        removal.error = Some(
            "The daemon restarted before it was confirmed; confirming again is safe, and \
             counts as done if it went through"
                .into(),
        );
    }
    assert_snapshot!(
        "event_cancel_failed",
        frame_snapshot(&mut app, WIDTH, HEIGHT)
    );

    app.overlay = Overlay::EventRemove(Box::new(EventRemoveOverlay {
        account: "evoqs".into(),
        event: "ev-holiday".into(),
        summary: "Sommerfest".into(),
        cancel: false,
        attendees: 0,
        comment: String::new(),
        error: None,
        scope: None,
    }));
    assert_snapshot!("event_delete", frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// Records every call and starts an operation for each.
#[derive(Default)]
struct Writes(Mutex<Vec<(String, Value)>>);

impl Queries for Writes {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let mut calls = self.0.lock().unwrap();
        calls.push((method.to_string(), params));
        Ok(json!({"operation_id": format!("op-{}", calls.len())}))
    }
}

/// `E` on a meeting the user was invited to opens the form on the
/// reminder, which with the show-as below it is all an attendee changes; a
/// changed title is named as the organizer's, and the reminder and the
/// show-as `Right` moved to are one `event.update` with those fields. A failed change reopens the form, and a settled one says so.
#[test]
fn an_attendee_edits_the_reminder_through_event_update() {
    let _data = seeded();
    let mut app = app_over(CallLog::default());
    app.switch_view(View::Calendar);
    assert_eq!(summaries(&app)[0], "Project review");
    app.handle_key(key('E'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("E opens the event form: {:?}", app.status_message);
    };
    assert!(form.editing.is_some());
    assert_eq!(form.row, EventFormRow::Reminder);
    assert_eq!(form.title, "Project review");

    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("an unchanged form stays open");
    };
    assert_eq!(form.error.as_deref(), Some("Nothing changed yet"));
    for c in "none".chars() {
        app.handle_key(key(c));
    }
    let Overlay::EventForm(form) = &mut app.overlay else {
        panic!("the form is open");
    };
    form.reminder = "10".into();
    form.title = "Mine now".into();
    assert!(form.problem().unwrap().starts_with("Only the organizer"));
    form.title = "Project review".into();
    app.handle_key(KeyEvent::from(KeyCode::Down));
    app.handle_key(KeyEvent::from(KeyCode::Right));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("the form is open");
    };
    assert_eq!(form.row, EventFormRow::ShowAs);
    assert_eq!(form.show_as_label(), "Out of office");
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Some(Action::UpdateEvent(form)) = app.pending_actions.pop_front() else {
        panic!("Enter queues the change");
    };

    let door = Writes::default();
    assert!(dispatch(&mut app, &door, &Action::UpdateEvent(form)));
    assert_eq!(
        door.0.lock().unwrap().clone(),
        [(
            "event.update".to_string(),
            json!({"account": ACCOUNT, "event": "ev-meeting",
                   "fields": {"reminder": 10, "show_as": "oof"}})
        )]
    );
    assert_eq!(app.bg_count, 1);

    let failed = settled(
        &Awaited::EventChange {
            operation_id: "op-1".into(),
            done: "Updated Project review".into(),
        },
        &json!({"operation_id": "op-1", "state": "failed",
                "error": {"code": -32603, "message": "Graph refused the update: HTTP 503"}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, failed);
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("a failed change reopens the form");
    };
    assert!(form.error.as_deref().unwrap().contains("HTTP 503"));
    assert_eq!(form.reminder, "10");
    let form = form.clone();
    app.overlay = Overlay::None;
    assert!(dispatch(&mut app, &door, &Action::UpdateEvent(form)));
    let done = settled(
        &Awaited::EventChange {
            operation_id: "op-2".into(),
            done: "Updated Project review".into(),
        },
        &json!({"operation_id": "op-2", "state": "succeeded", "result": {"account": ACCOUNT}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, done);
    assert!(app.calendar_view.pending_forms.is_empty());
    assert_eq!(
        app.status_message.as_deref(),
        Some("Updated Project review")
    );
}

/// `D` on someone else's meeting points at the answer that tells its
/// organizer, on an occurrence of it before any scope is asked, as the
/// desktop does; on an event that tells nobody it asks to delete it, and
/// `y` is one `event.delete`; `n` keeps the event.
#[test]
fn d_deletes_an_event_that_tells_nobody_and_refuses_a_live_meeting() {
    let _data = seeded();
    let mut app = app_over(CallLog::default());
    app.switch_view(View::Calendar);
    app.handle_key(key('D'));
    assert!(matches!(app.overlay, Overlay::None));
    let refusal =
        "Project review is someone else's meeting: decline it with V to tell the organizer";
    assert_eq!(app.status_message.as_deref(), Some(refusal));
    let mut occurrence = app.selected_event().cloned().expect("the meeting");
    occurrence.kind = "occurrence".into();
    occurrence.series_master_id = Some("ev-review-series".into());
    app.status_message = None;
    app.open_event_remove_for(&occurrence);
    assert!(
        matches!(app.overlay, Overlay::None),
        "no scope is asked of someone else's occurrence"
    );
    assert_eq!(app.status_message.as_deref(), Some(refusal));

    app.handle_key(key('j'));
    app.handle_key(key('D'));
    let Overlay::EventRemove(overlay) = &app.overlay else {
        panic!("D on the holiday asks to delete it");
    };
    assert!(!overlay.cancel);
    assert_eq!(overlay.event, "ev-holiday");
    app.handle_key(key('n'));
    assert!(matches!(app.overlay, Overlay::None));
    assert!(app.pending_actions.is_empty());

    app.handle_key(key('D'));
    app.handle_key(key('y'));
    let Some(Action::RemoveEvent(removal)) = app.pending_actions.pop_front() else {
        panic!("y queues the delete");
    };
    let door = Writes::default();
    assert!(dispatch(&mut app, &door, &Action::RemoveEvent(removal)));
    assert_eq!(
        door.0.lock().unwrap().clone(),
        [(
            "event.delete".to_string(),
            json!({"account": ACCOUNT, "event": "ev-holiday"})
        )]
    );
    let done = settled(
        &Awaited::EventChange {
            operation_id: "op-1".into(),
            done: "Deleted Holiday".into(),
        },
        &json!({"operation_id": "op-1", "state": "succeeded",
                "result": {"account": ACCOUNT, "event": "ev-holiday"}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, done);
    assert_eq!(app.status_message.as_deref(), Some("Deleted Holiday"));
}

/// The cancel confirmation of a meeting the user organizes takes a comment
/// as typed, and `Enter` is one `event.cancel` with it; the settled cancel
/// names who was told.
#[test]
fn a_cancel_sends_its_comment_through_event_cancel() {
    let mut app = App::default_for_tests();
    app.overlay = Overlay::EventRemove(Box::new(EventRemoveOverlay {
        account: ACCOUNT.into(),
        event: "ev-own".into(),
        summary: "Budget".into(),
        cancel: true,
        attendees: 1,
        comment: String::new(),
        error: None,
        scope: None,
    }));
    for c in "Postponed".chars() {
        app.handle_key(key(c));
    }
    app.handle_key(KeyEvent::from(KeyCode::Backspace));
    app.handle_key(key('d'));
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Some(Action::RemoveEvent(removal)) = app.pending_actions.pop_front() else {
        panic!("Enter queues the cancel");
    };
    let door = Writes::default();
    assert!(dispatch(&mut app, &door, &Action::RemoveEvent(removal)));
    assert_eq!(
        door.0.lock().unwrap().clone(),
        [(
            "event.cancel".to_string(),
            json!({"account": ACCOUNT, "event": "ev-own", "comment": "Postponed"})
        )]
    );
    let done = settled(
        &Awaited::EventChange {
            operation_id: "op-1".into(),
            done: "Cancelled Budget".into(),
        },
        &json!({"operation_id": "op-1", "state": "succeeded",
                "result": {"account": ACCOUNT, "event": "ev-own",
                           "notified": ["ada@example.com"]}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, done);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Cancelled Budget; told ada@example.com")
    );
}

/// A cancel the server failed asks again, as the desktop's does: the
/// confirmation comes back with the comment as typed and the reason under
/// it, and `Enter` sends the same cancel. One that fails while another
/// overlay is open is kept for `D` on the event, which the status line
/// says, and a refusal at the call asks again at once.
#[test]
fn a_failed_cancel_asks_again_with_its_comment() {
    let mut app = App::default_for_tests();
    let removal = EventRemoveOverlay {
        account: ACCOUNT.into(),
        event: "ev-own".into(),
        summary: "Budget".into(),
        cancel: true,
        attendees: 2,
        comment: "Postponed".into(),
        error: None,
        scope: None,
    };
    let door = Writes::default();
    assert!(dispatch(
        &mut app,
        &door,
        &Action::RemoveEvent(Box::new(removal.clone()))
    ));
    assert_eq!(app.calendar_view.pending_removals.len(), 1);
    let failed = settled(
        &Awaited::EventChange {
            operation_id: "op-1".into(),
            done: "Cancelled Budget".into(),
        },
        &json!({"operation_id": "op-1", "state": "failed",
                "error": {"code": -32603, "message": "Graph refused the cancel: HTTP 503"}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, failed);
    assert!(app.calendar_view.pending_removals.is_empty());
    let Overlay::EventRemove(again) = &app.overlay else {
        panic!("a failed cancel asks again: {:?}", app.status_message);
    };
    assert_eq!(again.comment, "Postponed");
    let why = again.error.clone().unwrap();
    assert!(why.starts_with("Cancelling Budget failed"), "{why}");
    assert!(why.contains("HTTP 503"), "{why}");

    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Some(Action::RemoveEvent(retry)) = app.pending_actions.pop_front() else {
        panic!("Enter sends the cancel again");
    };
    assert_eq!(retry.comment, "Postponed");
    assert!(dispatch(&mut app, &door, &Action::RemoveEvent(retry)));
    assert_eq!(
        door.0.lock().unwrap()[1],
        (
            "event.cancel".to_string(),
            json!({"account": ACCOUNT, "event": "ev-own", "comment": "Postponed"})
        )
    );

    app.overlay = Overlay::Help;
    let failed = settled(
        &Awaited::EventChange {
            operation_id: "op-2".into(),
            done: "Cancelled Budget".into(),
        },
        &json!({"operation_id": "op-2", "state": "failed",
                "error": {"code": -32603, "message": "HTTP 503"}}),
    );
    crate::tui::bg::handle_bg_result(&mut app, failed);
    assert!(matches!(app.overlay, Overlay::Help));
    assert!(app
        .status_message
        .as_deref()
        .unwrap()
        .ends_with("; D on Budget asks again"));
    assert_eq!(
        app.calendar_view
            .kept_removal
            .as_ref()
            .map(|kept| kept.comment.as_str()),
        Some("Postponed")
    );

    struct Refuses;
    impl Queries for Refuses {
        fn call(&self, _method: &str, _params: Value) -> anyhow::Result<Value> {
            anyhow::bail!("The calendar Calendar of evoqs is read-only")
        }
    }
    app.overlay = Overlay::None;
    assert!(dispatch(
        &mut app,
        &Refuses,
        &Action::RemoveEvent(Box::new(removal))
    ));
    let Overlay::EventRemove(again) = &app.overlay else {
        panic!("a refusal asks again");
    };
    assert!(again.error.as_deref().unwrap().contains("read-only"));
    assert!(app.calendar_view.pending_removals.is_empty());
}

// ---------------------------------------------------------------------------
// Recurrence and scopes (PERSO-255)
// ---------------------------------------------------------------------------

/// The event form making a weekly series, its Repeat row naming the rule in
/// words over the rows that shape it; the chooser `E` asks on an occurrence
/// of the jour fixe; and the form on that occurrence alone, whose Repeat row
/// names its series.
#[test]
fn golden_calendar_series_forms() {
    let mut app = calendar_fixture();
    let choice = EventCalendarChoice {
        account: "evoqs".into(),
        id: "cal-main".into(),
        name: "Kalender".into(),
        teams: true,
    };
    let day = chrono::NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();
    let mut form = EventForm::new(vec![choice.clone()], "evoqs", day, "Europe/Berlin");
    form.title = "Standup".into();
    form.start = "09:00".into();
    form.end = "09:15".into();
    form.cycle_repeat(true);
    form.cycle_repeat(true);
    form.repeat_on = "Tu, Th".into();
    form.ends = "2026-09-30".into();
    form.row = EventFormRow::Repeat;
    assert_eq!(
        form.repeat_label(),
        "Weekly on Tuesday, Thursday until 2026-09-30"
    );
    app.overlay = Overlay::EventForm(Box::new(form));
    assert_snapshot!("event_form_repeat", frame_snapshot(&mut app, WIDTH, HEIGHT));

    let mut jour = app
        .calendar_view
        .events
        .iter()
        .find(|e| e.id == "ev-jour-1")
        .cloned()
        .unwrap();
    jour.is_organizer = true;
    jour.own_response = "organizer".into();
    app.overlay = Overlay::EventScope(Box::new(EventScopeChooser {
        event: jour.clone(),
        remove: false,
        series: false,
    }));
    assert_snapshot!("event_scope", frame_snapshot(&mut app, WIDTH, HEIGHT));

    let form = EventForm::edit_occurrence(&jour, choice, app.calendar_view.zone);
    assert!(!form.rows().contains(&EventFormRow::Every));
    app.overlay = Overlay::EventForm(Box::new(form));
    assert_snapshot!(
        "event_form_occurrence",
        frame_snapshot(&mut app, WIDTH, HEIGHT)
    );
}

/// [`server`] plus a weekly series the user organizes, its master in the
/// series cache and two occurrences, five and twelve days out.
fn server_with_series() -> FakeCalendarBackend {
    use crate::calendar_backend::SeriesInfo;
    use crate::calendar_model::{Frequency, Recurrence, Weekday};
    let time = |days| EventTime::timed(at(days, 8), at(days, 9), "Europe/Berlin").unwrap();
    let mut master = server_event("ev-weekly", "cal-main", "Weekly sync", time(5));
    master.kind = EventKind::SeriesMaster;
    master.is_organizer = true;
    master.own_response = OwnResponse::Organizer;
    let rule = Recurrence {
        frequency: Frequency::Weekly,
        interval: 1,
        weekdays: vec![Weekday::of_date(at(5, 0).date_naive())],
        day_of_month: None,
        week_index: None,
        month: None,
        first_day_of_week: Some(Weekday::Monday),
        start: Some(at(5, 0).date_naive()),
        until: None,
        count: Some(2),
    };
    master.recurrence = Some(rule.describe());
    let fake = server();
    for (id, days) in [("ev-weekly-a", 5), ("ev-weekly-b", 12)] {
        let mut occurrence = master.clone();
        occurrence.id = id.into();
        occurrence.kind = EventKind::Occurrence;
        occurrence.series_master_id = Some("ev-weekly".into());
        occurrence.time = time(days);
        occurrence.recurrence_id = Some(at(days, 8).to_rfc3339());
        fake.put_event(occurrence);
    }
    fake.with_series(vec![SeriesInfo {
        master_id: "ev-weekly".into(),
        uid: "ev-weekly@example.com".into(),
        recurrence: Some(rule),
        master: Some(Box::new(master)),
        zone: Some("Europe/Berlin".into()),
    }])
}

/// `E` on an occurrence asks its scope first; `s` reads the series' master
/// through `event.get` and opens the form on it, its rule in the Repeat
/// rows, and `Enter` is one `event.update` of the master in the series
/// scope. `D` on an occurrence asks too, and `o` then `y` delete that
/// occurrence alone.
#[test]
fn an_occurrence_asks_its_scope_before_a_change() {
    let data = crate::config::test_env::TestDataDir::new();
    std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(sync_account(
            &store,
            &server_with_series().serving(ACCOUNT),
            ACCOUNT,
            Utc::now(),
        ))
        .expect("the fake server syncs");
    let log = CallLog::default();
    let mut app = app_over(log.clone());
    app.switch_view(View::Calendar);
    let at_weekly = summaries(&app)
        .iter()
        .position(|s| s == "Weekly sync")
        .expect("the series shows");
    for _ in 0..at_weekly {
        app.handle_key(key('j'));
    }

    app.handle_key(key('E'));
    let Overlay::EventScope(chooser) = &app.overlay else {
        panic!(
            "E on an occurrence asks its scope: {:?}",
            app.status_message
        );
    };
    assert!(!chooser.remove && !chooser.series);
    app.handle_key(key('s'));
    let Overlay::EventForm(form) = &app.overlay else {
        panic!("s opens the series form: {:?}", app.status_message);
    };
    assert_eq!(form.scope(), Some("series"));
    assert_eq!(form.editing.as_ref().unwrap().event, "ev-weekly");
    assert_eq!(form.repeat, Some(crate::tui::app::RepeatPattern::Weekly));
    assert_eq!(form.ends, "2");
    assert!(
        form.repeat_label().ends_with("(2 times)"),
        "{}",
        form.repeat_label()
    );
    assert_eq!(
        calls(&log, "event.get"),
        [json!({"account": ACCOUNT, "event": "ev-weekly"})]
    );
    let Overlay::EventForm(form) = &mut app.overlay else {
        unreachable!()
    };
    form.title = "Weekly sync, new room".into();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let Some(Action::UpdateEvent(form)) = app.pending_actions.pop_front() else {
        panic!("Enter queues the change");
    };
    let door = Writes::default();
    assert!(dispatch(&mut app, &door, &Action::UpdateEvent(form)));

    app.handle_key(key('D'));
    let Overlay::EventScope(chooser) = &app.overlay else {
        panic!("D on an occurrence asks its scope");
    };
    assert!(chooser.remove);
    app.handle_key(key('o'));
    let Overlay::EventRemove(removal) = &app.overlay else {
        panic!("o asks to delete the occurrence");
    };
    assert_eq!(removal.scope.as_deref(), Some("occurrence"));
    assert!(!removal.cancel);
    app.handle_key(key('y'));
    let Some(Action::RemoveEvent(removal)) = app.pending_actions.pop_front() else {
        panic!("y queues the delete");
    };
    assert!(dispatch(&mut app, &door, &Action::RemoveEvent(removal)));
    assert_eq!(
        door.0.lock().unwrap().clone(),
        [
            (
                "event.update".to_string(),
                json!({"account": ACCOUNT, "event": "ev-weekly", "scope": "series",
                       "fields": {"summary": "Weekly sync, new room"}})
            ),
            (
                "event.delete".to_string(),
                json!({"account": ACCOUNT, "event": "ev-weekly-a", "scope": "occurrence"})
            ),
        ]
    );
    drop(data);
}
