//! An account whose store the daemon is still opening (PERSO-109), in the
//! TUI: switching to it shows it opening, as the start screen does, rather
//! than an error line, and its `account.state_changed` to `ready` loads what
//! the switch left empty: the open mailbox, the Contacts view and the
//! calendar, which the daemon answered without that account meanwhile.
//!
//! The daemon is a door that answers the reads a ready event makes and
//! refuses the ones an opening account refuses, recording every call.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use mp_protocol::state::{AccountSnapshot, AccountState as WireAccountState, Bootstrap, Snapshot};
use mp_protocol::EventEnvelope;

use crate::app::{Action, App, StatusLevel, View};

const INSTANCE: &str = "opening-tests";
const READY: &str = "work";
const OPENING: &str = "home";

type Calls = Arc<Mutex<Vec<String>>>;

/// The daemon: `home` refuses its store reads while `opening` holds.
#[derive(Clone, Default)]
struct Daemon {
    opening: Arc<Mutex<bool>>,
    calls: Calls,
}

impl crate::queries::Queries for Daemon {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(method.to_string());
        let account = params["account"].as_str().unwrap_or_default().to_string();
        if account == OPENING && *self.opening.lock().unwrap() && method != "account.credentials" {
            return Err(mp_client::session::Refused {
                method: method.to_string(),
                error: mp_protocol::RpcError {
                    code: mp_protocol::ErrorCode::AccountNotReady.code(),
                    message: format!(
                        "{OPENING} is still opening its store; try again once it is ready"
                    ),
                    data: Some(json!({"account": OPENING, "state": "opening"})),
                },
            }
            .into());
        }
        match method {
            "mailbox.list" => Ok(json!({"account": account, "mailboxes": []})),
            "calendar.list" => Ok(json!({"calendars": []})),
            "calendar.view" => Ok(json!({
                "range": {"start": "2026-11-09T00:00:00Z", "end": "2026-11-16T00:00:00Z"},
                "zone": "UTC",
                "window": {"start": "2026-10-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"},
                "events": [],
            })),
            other => anyhow::bail!("the opening door answers no {other}"),
        }
    }
}

impl Daemon {
    fn calls(&self, method: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|called| called.as_str() == method)
            .count()
    }
}

fn config() -> mp_core::config::GlobalConfig {
    let account = |name: &str| mp_core::config::AccountConfig {
        name: name.to_string(),
        ..Default::default()
    };
    mp_core::config::GlobalConfig {
        accounts: vec![account(READY), account(OPENING)],
        ..Default::default()
    }
}

/// `work` ready and `home` opening, as a daemon that has just started
/// reports them.
fn bootstrap() -> Bootstrap {
    let account = |name: &str, state| AccountSnapshot {
        name: name.to_string(),
        state,
        ..Default::default()
    };
    Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision: 1,
        capabilities: vec!["state.bootstrap".to_string()],
        connection: 1,
        snapshot: Snapshot {
            accounts: vec![
                account(READY, WireAccountState::Ready),
                account(OPENING, WireAccountState::Opening),
            ],
            mailboxes: BTreeMap::new(),
            ..Default::default()
        },
    }
}

fn app() -> (App, Daemon) {
    let daemon = Daemon::default();
    *daemon.opening.lock().unwrap() = true;
    let served = daemon.clone();
    let mut app = App::from_bootstrap(config(), &bootstrap());
    app.session = Some(crate::session::Session::serving(move || served));
    app.pending_actions.clear();
    assert!(!app.accounts[0].opening, "{READY} is ready");
    assert!(app.accounts[1].opening, "{OPENING} is opening");
    (app, daemon)
}

/// `home`'s store is open: the daemon serves it and says so.
fn ready(app: &mut App, daemon: &Daemon) {
    *daemon.opening.lock().unwrap() = false;
    app.apply_event(&EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision: 2,
        kind: mp_protocol::events::KIND_ACCOUNT_STATE_CHANGED.to_string(),
        payload: json!({"account": OPENING, "state": "ready"}),
    });
}

fn queued(app: &App, wanted: impl Fn(&Action) -> bool) -> bool {
    app.pending_actions.iter().any(wanted)
}

/// The whole frame, row by row.
fn drawn(app: &mut App) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).expect("a terminal");
    terminal
        .draw(|frame| crate::ui::view(app, frame))
        .expect("a frame");
    let buffer = terminal.backend().buffer();
    let area = *buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the last status line is a progress line, not an error.
fn says_progress(app: &App) -> bool {
    matches!(
        app.status_log.back().map(|entry| &entry.level),
        Some(StatusLevel::Progress)
    )
}

/// The mail view: the switch says the account is opening and loads nothing,
/// and the ready event loads the open mailbox.
#[test]
fn switching_to_an_opening_account_shows_it_opening_and_its_ready_loads_the_list() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();

    app.switch_account(1);
    assert_eq!(app.status_message.as_deref(), Some("Opening home..."));
    assert!(says_progress(&app), "{:?}", app.status_message);
    assert!(
        !queued(&app, |a| matches!(a, Action::LoadMailbox { .. })),
        "nothing is loaded from an account still opening: {:?}",
        app.pending_actions
    );

    ready(&mut app, &daemon);
    assert!(!app.accounts[1].opening);
    assert!(
        queued(&app, |a| matches!(a, Action::LoadMailbox { .. })),
        "the ready event loads the open mailbox: {:?}",
        app.pending_actions
    );
}

/// The Contacts view: the switch says the account is opening rather than
/// asking for its contacts and landing the refusal as an error line, and the
/// ready event asks for them.
#[test]
fn switching_the_contacts_view_to_an_opening_account_shows_it_opening_and_its_ready_loads_them() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();
    app.switch_view(View::Contacts);
    app.pending_actions.clear();

    app.switch_account(1);
    assert!(
        !queued(&app, |a| matches!(a, Action::LoadContacts { .. })),
        "the contacts of an account still opening are not asked for: {:?}",
        app.pending_actions
    );
    assert_eq!(app.status_message.as_deref(), Some("Opening home..."));
    assert!(says_progress(&app), "{:?}", app.status_message);
    let screen = drawn(&mut app);
    assert!(
        !screen.contains("No contact cache yet"),
        "the pane does not call the cache missing: {screen}"
    );

    ready(&mut app, &daemon);
    assert!(
        queued(&app, |a| matches!(a, Action::LoadContacts { .. })),
        "the ready event asks for the contacts: {:?}",
        app.pending_actions
    );
}

/// The calendar shows every account's events, and the daemon answers it
/// without an account still opening: the ready event of any account reads it
/// again when it is loaded.
#[test]
fn an_account_turning_ready_reads_the_loaded_calendar_again() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();
    app.load_calendar();
    assert_eq!(daemon.calls("calendar.view"), 1);

    ready(&mut app, &daemon);
    assert_eq!(
        daemon.calls("calendar.view"),
        2,
        "the calendar is read again once {OPENING} serves"
    );
}
