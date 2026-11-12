//! An account whose IMAP login the server rejected (PERSO-296), in the TUI.
//!
//! The daemon holds the password it was refused and says so twice: the
//! `account:<name>` health check turns `warn`, published as a
//! `diagnostic.check_changed`, and `account.credentials` answers
//! `login_rejected`. The TUI reads the second when the first moves, and marks
//! the account in the sidebar and the account picker with the sentence
//! `mp daemon health` prints, which names the command that sets the password.
//!
//! The daemon is a door that answers `account.credentials` from a flag the
//! row flips, and `state.bootstrap` for the resync row.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use mp_protocol::diagnostic::{CheckStatus, HealthCheck};
use mp_protocol::state::{AccountSnapshot, AccountState as WireAccountState, Bootstrap, Snapshot};
use mp_protocol::EventEnvelope;

use crate::app::{AccountPicker, App, Overlay};
use crate::events::{drain, Incoming};

const INSTANCE: &str = "login-rejected-tests";
const ACCOUNT: &str = "home";

/// The daemon: `account.credentials` answers `login_rejected` from `rejected`.
#[derive(Clone, Default)]
struct Daemon {
    rejected: Arc<Mutex<bool>>,
    diagnostics: Arc<Mutex<Vec<HealthCheck>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl crate::queries::Queries for Daemon {
    fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(method.to_string());
        match method {
            "account.credentials" => Ok(json!({"accounts": [{
                "account": ACCOUNT,
                "auth_method": "password",
                "smtp_password": true,
                "imap_password": true,
                "token": null,
                "missing_scopes": [],
                "login_rejected": *self.rejected.lock().unwrap(),
            }]})),
            "state.bootstrap" => {
                let mut bootstrap = bootstrap(5);
                bootstrap.snapshot.diagnostics = self.diagnostics.lock().unwrap().clone();
                Ok(serde_json::to_value(bootstrap)?)
            }
            other => anyhow::bail!("the login door answers no {other}"),
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
    mp_core::config::GlobalConfig {
        accounts: vec![mp_core::config::AccountConfig {
            name: ACCOUNT.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn bootstrap(revision: u64) -> Bootstrap {
    Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision,
        capabilities: vec!["state.bootstrap".to_string()],
        connection: 1,
        snapshot: Snapshot {
            accounts: vec![AccountSnapshot {
                name: ACCOUNT.to_string(),
                state: WireAccountState::Ready,
                ..Default::default()
            }],
            mailboxes: BTreeMap::new(),
            ..Default::default()
        },
    }
}

/// `home` ready, its credentials read once with nothing rejected.
fn app() -> (App, Daemon) {
    let daemon = Daemon::default();
    let served = daemon.clone();
    let mut app = App::from_bootstrap(config(), &bootstrap(1));
    app.session = Some(crate::session::Session::serving(move || served));
    app.pending_actions.clear();
    app.refresh_credentials(0);
    assert!(!rejected(&app));
    (app, daemon)
}

/// The `account:home` check as the daemon publishes it.
fn account_check(status: CheckStatus) -> HealthCheck {
    let detail = match status {
        CheckStatus::Ok => "serving".to_string(),
        _ => mp_client::format::login_rejected_detail(ACCOUNT),
    };
    HealthCheck {
        name: format!("account:{ACCOUNT}"),
        status,
        detail,
    }
}

fn check_changed(revision: u64, check: &HealthCheck) -> EventEnvelope {
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision,
        kind: mp_protocol::events::KIND_DIAGNOSTIC_CHECK_CHANGED.to_string(),
        payload: serde_json::to_value(check).unwrap(),
    }
}

fn rejected(app: &App) -> bool {
    app.accounts[0]
        .credentials
        .is_some_and(|credentials| credentials.login_rejected)
}

/// The whole frame, row by row.
fn drawn(app: &mut App) -> String {
    app.terminal_width = 140;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).expect("a terminal");
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

/// The account check turning `warn` reads `account.credentials` again, and
/// the sidebar and the picker say the server rejected the password and name
/// the command that sets it; the check clearing takes the mark away.
#[test]
fn a_rejected_login_marks_the_account_until_its_check_clears() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();

    *daemon.rejected.lock().unwrap() = true;
    app.apply_event(&check_changed(2, &account_check(CheckStatus::Warn)));
    assert!(rejected(&app), "the check's change reads the credentials");

    let screen = drawn(&mut app);
    assert!(screen.contains("login rejected"), "{screen}");
    assert!(
        screen.contains("the server rejected the"),
        "the sidebar says why: {screen}"
    );
    assert!(
        screen.contains("set-password -A home imap"),
        "and names the way out: {screen}"
    );

    app.overlay = Overlay::Account(AccountPicker { selected: 0 });
    let picker = drawn(&mut app);
    assert!(
        picker.contains("home  login rejected"),
        "the picker marks the account: {picker}"
    );
    app.overlay = Overlay::None;

    *daemon.rejected.lock().unwrap() = false;
    app.apply_event(&check_changed(3, &account_check(CheckStatus::Ok)));
    assert!(!rejected(&app), "the recovery reads them again");
    let screen = drawn(&mut app);
    assert!(!screen.contains("login rejected"), "{screen}");
}

/// A check about something else reads nothing: only an account's own check
/// can move its login.
#[test]
fn a_check_that_names_no_account_reads_no_credentials() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();
    let before = daemon.calls("account.credentials");
    app.apply_event(&check_changed(
        2,
        &HealthCheck {
            name: "config_loaded".to_string(),
            status: CheckStatus::Warn,
            detail: "an ignored key".to_string(),
        },
    ));
    assert_eq!(daemon.calls("account.credentials"), before);
}

/// A resync lands the snapshot's checks without their events, so an account
/// it reports rejected, or one held as rejected that it no longer reports
/// (a restarted daemon has forgotten the refusal), is read again.
#[test]
fn a_resync_reads_the_credentials_of_an_account_whose_login_moved() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();
    let (sender, events) = mpsc::channel();

    *daemon.rejected.lock().unwrap() = true;
    *daemon.diagnostics.lock().unwrap() = vec![account_check(CheckStatus::Warn)];
    sender
        .send(Incoming::Resync {
            instance_id: INSTANCE.to_string(),
            reason: "event_queue_overflow".to_string(),
        })
        .unwrap();
    drain(&mut app, &daemon, &events);
    assert!(rejected(&app), "the snapshot's check reads the credentials");

    *daemon.rejected.lock().unwrap() = false;
    daemon.diagnostics.lock().unwrap().clear();
    sender
        .send(Incoming::Resync {
            instance_id: INSTANCE.to_string(),
            reason: "event_queue_overflow".to_string(),
        })
        .unwrap();
    drain(&mut app, &daemon, &events);
    assert!(
        !rejected(&app),
        "a rejection the snapshot no longer reports is read again"
    );
}
