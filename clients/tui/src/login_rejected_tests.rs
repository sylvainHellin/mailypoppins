//! An account whose IMAP or SMTP login the server rejected, or whose Graph
//! sign-in Microsoft refused (PERSO-296), in the TUI.
//!
//! The daemon holds the password it was refused and says so twice: the
//! `account:<name>` health check turns `warn`, published as a
//! `diagnostic.check_changed`, and `account.credentials` answers
//! `rejected_logins`. The TUI reads the second when the first moves, and
//! marks the account in the sidebar and the account picker with the sentence
//! `mp daemon health` prints, which names the login and the command that
//! fixes it.
//!
//! The daemon is a door that answers `account.credentials` from a flag the
//! row flips, and `state.bootstrap` for the resync row.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use mp_protocol::account::RejectedLogin;
use mp_protocol::diagnostic::{CheckStatus, HealthCheck};
use mp_protocol::state::{AccountSnapshot, AccountState as WireAccountState, Bootstrap, Snapshot};
use mp_protocol::EventEnvelope;

use crate::app::{AccountPicker, App, Overlay};
use crate::events::{drain, Incoming};

const INSTANCE: &str = "login-rejected-tests";
const ACCOUNT: &str = "home";

/// The daemon: `account.credentials` answers `rejected_logins` from
/// `rejected`.
#[derive(Clone, Default)]
struct Daemon {
    rejected: Arc<Mutex<Option<RejectedLogin>>>,
    /// What `shared_password` answers: the IMAP and SMTP logins use one
    /// password (PERSO-297).
    shared: Arc<Mutex<bool>>,
    diagnostics: Arc<Mutex<Vec<HealthCheck>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl crate::queries::Queries for Daemon {
    fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(method.to_string());
        match method {
            "account.credentials" => Ok(json!({"accounts": [{
                "account": ACCOUNT,
                // A refused sign-in is a Graph account's.
                "auth_method": match *self.rejected.lock().unwrap() {
                    Some(RejectedLogin::Graph) => "graph",
                    _ => "password",
                },
                "smtp_password": true,
                "imap_password": true,
                "shared_password": *self.shared.lock().unwrap(),
                "token": null,
                "missing_scopes": [],
                "rejected_logins": self.rejected.lock().unwrap().iter().collect::<Vec<_>>(),
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
    assert_eq!(rejected(&app), None);
    (app, daemon)
}

/// The `account:home` check as the daemon publishes it, with the IMAP login
/// rejected unless it is `ok`.
fn account_check(status: CheckStatus) -> HealthCheck {
    let detail = match status {
        CheckStatus::Ok => "serving".to_string(),
        _ => mp_core::mp_command::login_rejected_detail(ACCOUNT, RejectedLogin::Imap, false),
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

fn rejected(app: &App) -> Option<RejectedLogin> {
    app.accounts[0]
        .credentials
        .and_then(|credentials| credentials.rejected_login)
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

    *daemon.rejected.lock().unwrap() = Some(RejectedLogin::Imap);
    app.apply_event(&check_changed(2, &account_check(CheckStatus::Warn)));
    assert_eq!(
        rejected(&app),
        Some(RejectedLogin::Imap),
        "the check's change reads the credentials"
    );

    let screen = drawn(&mut app);
    assert!(screen.contains("login rejected"), "{screen}");
    assert!(
        screen.contains("the server rejected the IMAP"),
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

    *daemon.rejected.lock().unwrap() = None;
    app.apply_event(&check_changed(3, &account_check(CheckStatus::Ok)));
    assert_eq!(rejected(&app), None, "the recovery reads them again");
    let screen = drawn(&mut app);
    assert!(!screen.contains("login rejected"), "{screen}");
}

/// A rejected SMTP login marks the account the same way, and the sidebar
/// names the SMTP password and the command that sets it.
#[test]
fn a_rejected_smtp_login_names_the_smtp_password() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();

    *daemon.rejected.lock().unwrap() = Some(RejectedLogin::Smtp);
    app.apply_event(&check_changed(2, &account_check(CheckStatus::Warn)));
    assert_eq!(rejected(&app), Some(RejectedLogin::Smtp));

    let screen = drawn(&mut app);
    assert!(screen.contains("login rejected"), "{screen}");
    assert!(
        screen.contains("the server rejected the SMTP"),
        "the sidebar names the login: {screen}"
    );
    assert!(
        screen.contains("set-password -A home smtp"),
        "and the way out: {screen}"
    );
}

/// An account whose IMAP and SMTP logins use one password (PERSO-297) is
/// told the command without a kind, which sets that one password for both.
#[test]
fn a_rejected_shared_password_names_the_command_that_sets_both() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();

    *daemon.shared.lock().unwrap() = true;
    *daemon.rejected.lock().unwrap() = Some(RejectedLogin::Imap);
    app.apply_event(&check_changed(2, &account_check(CheckStatus::Warn)));

    let screen = drawn(&mut app);
    assert!(
        screen.contains("set-password -A home"),
        "the sidebar names the way out: {screen}"
    );
    assert!(
        !screen.contains("set-password -A home imap"),
        "without a kind: {screen}"
    );
}

/// A Graph account whose sign-in Microsoft refused is marked the same way,
/// and the sidebar names the command that signs it in again.
#[test]
fn a_refused_sign_in_names_the_command_that_signs_in_again() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let (mut app, daemon) = app();

    *daemon.rejected.lock().unwrap() = Some(RejectedLogin::Graph);
    app.apply_event(&check_changed(2, &account_check(CheckStatus::Warn)));
    assert_eq!(rejected(&app), Some(RejectedLogin::Graph));

    let screen = drawn(&mut app);
    assert!(screen.contains("login rejected"), "{screen}");
    assert!(
        screen.contains("the sign-in expired or was"),
        "the sidebar says why: {screen}"
    );
    assert!(
        screen.contains("oauth2-login -A home"),
        "and the way out: {screen}"
    );
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

    *daemon.rejected.lock().unwrap() = Some(RejectedLogin::Imap);
    *daemon.diagnostics.lock().unwrap() = vec![account_check(CheckStatus::Warn)];
    sender
        .send(Incoming::Resync {
            instance_id: INSTANCE.to_string(),
            reason: "event_queue_overflow".to_string(),
        })
        .unwrap();
    drain(&mut app, &daemon, &events);
    assert!(
        rejected(&app).is_some(),
        "the snapshot's check reads the credentials"
    );

    *daemon.rejected.lock().unwrap() = None;
    daemon.diagnostics.lock().unwrap().clear();
    sender
        .send(Incoming::Resync {
            instance_id: INSTANCE.to_string(),
            reason: "event_queue_overflow".to_string(),
        })
        .unwrap();
    drain(&mut app, &daemon, &events);
    assert_eq!(
        rejected(&app),
        None,
        "a rejection the snapshot no longer reports is read again"
    );
}
