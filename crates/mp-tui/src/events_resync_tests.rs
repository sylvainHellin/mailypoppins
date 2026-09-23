//! What a resync costs an account that had already opened (Phase 5 review,
//! ticket #0124).
//!
//! `src/tui/events_tests.rs` is P5-U7's contract module and is not edited, so
//! the row that pins the review finding lives here, in the same style: an
//! `App` with an account that opened long ago, one `Incoming::Resync` on a
//! channel, and one `drain` against a door that answers `state.bootstrap`.
//!
//! The finding: `apply_bootstrap` skips an account whose `opening` is clear,
//! and the drain sets the watermark past every event that would have corrected
//! that account's mailboxes, counts and cached listings. A client that
//! recovered from an overflow therefore kept showing the numbers it had before
//! it, for ever. [`App::apply_resync_bootstrap`] is the entry the drain uses
//! now, and this is what it has to be true of.
//!
//! The door is a hand-written [`Queries`] rather than the daemon fixture next
//! door: what is under test is which snapshot the model takes, not which bytes
//! a daemon produces, and a canned answer names the counts the assertion is
//! about.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::Arc;

use serde_json::Value;

use mp_protocol::state::{
    AccountSnapshot, AccountState as WireAccountState, Bootstrap, MailboxRow, Snapshot,
};

use crate::app::{Action, App};
use crate::events::{drain, Incoming};
use crate::queries::Queries;

/// The one account this module configures.
const ACCOUNT: &str = "alice";

/// A door that answers `state.bootstrap` with [`snapshot`] and records what it
/// was asked, so the row can say the resync cost exactly one call.
struct Door {
    bootstrap: Bootstrap,
    calls: RefCell<Vec<String>>,
}

impl Queries for Door {
    fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
        self.calls.borrow_mut().push(method.to_string());
        match method {
            "state.bootstrap" => Ok(serde_json::to_value(&self.bootstrap)?),
            other => anyhow::bail!("the resync door answers no {other}"),
        }
    }
}

/// One mailbox row.
fn row(role: &str, slug: &str, label: &str, total: u64) -> MailboxRow {
    MailboxRow {
        role: role.to_string(),
        slug: slug.to_string(),
        label: label.to_string(),
        total,
        unread: 0,
        badge: 0,
    }
}

/// A ready snapshot of [`ACCOUNT`] at `revision`, with the four role
/// mailboxes and `inbox` carrying `inbox_total`.
fn snapshot(revision: u64, inbox_total: u64) -> Bootstrap {
    let rows = vec![
        row("inbox", "inbox", "Inbox", inbox_total),
        row("drafts", "drafts", "Drafts", 0),
        row("sent", "sent", "Sent", 0),
        row("archive", "archive", "Archive", 0),
    ];
    Bootstrap {
        instance_id: "resync".to_string(),
        revision,
        capabilities: vec!["state.bootstrap".to_string()],
        snapshot: Snapshot {
            accounts: vec![AccountSnapshot {
                name: ACCOUNT.to_string(),
                state: WireAccountState::Ready,
                ..Default::default()
            }],
            mailboxes: BTreeMap::from([(ACCOUNT.to_string(), rows)]),
            ..Default::default()
        },
    }
}

/// The configuration the `App` is built from: one account, [`ACCOUNT`].
fn config() -> mp_core::config::GlobalConfig {
    mp_core::config::GlobalConfig {
        accounts: vec![mp_core::config::AccountConfig {
            name: ACCOUNT.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// After a `state.resync_required`, an account that had already opened takes
/// the new bootstrap's counts, loses its cached listings, and reloads the open
/// mailbox.
///
/// Before the fix the drain landed the snapshot through `apply_bootstrap`,
/// which skips an account whose `opening` is clear, while the same call moved
/// the watermark past every event that would have refreshed it: the sidebar
/// kept the pre-overflow numbers and the open list kept its cached rows, with
/// nothing left to correct either.
#[test]
fn a_resync_replaces_the_counts_of_an_account_that_had_already_opened() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let mut app = App::from_bootstrap(config(), &snapshot(1, 5));

    // What the account looks like after it opened and a few events landed:
    // the marker is clear, the counts are its own, and the open mailbox has a
    // cached listing.
    app.accounts[0].opening = false;
    app.accounts[0].mailbox_counts = vec![9, 9, 9, 9];
    app.mailbox_counts = vec![9, 9, 9, 9];
    app.email_cache[app.active_mailbox] = Some(Arc::new(Vec::new()));
    app.accounts[0].email_cache = app.email_cache.clone();
    app.pending_actions.clear();

    let door = Door {
        bootstrap: snapshot(42, 7),
        calls: RefCell::new(Vec::new()),
    };
    let (sender, events) = mpsc::channel();
    sender
        .send(Incoming::Resync {
            instance_id: "resync".to_string(),
            reason: "event_queue_overflow".to_string(),
        })
        .expect("the drain has not dropped the stream");

    assert_eq!(drain(&mut app, &door, &events), 1);

    assert_eq!(
        door.calls.borrow().as_slice(),
        ["state.bootstrap".to_string()],
        "a resync is one bootstrap and no second query on the UI thread"
    );
    assert_eq!(
        app.mailbox_counts,
        vec![7, 0, 0, 0],
        "the counts are the new snapshot's, not the ones from before the gap"
    );
    assert_eq!(
        app.accounts[0].mailbox_counts,
        vec![7, 0, 0, 0],
        "and the account behind the live view agrees with it"
    );
    assert!(
        app.email_cache.iter().all(Option::is_none),
        "every cached listing predates the gap, so none of them survives it"
    );
    assert!(
        app.pending_actions
            .iter()
            .any(|action| matches!(action, Action::LoadMailbox { .. })),
        "the open mailbox reloads off the UI thread, as a mailbox switch does"
    );
}

/// One armed hold under `instance_id`, as the daemon frames its start.
fn hold_started(instance_id: &str, revision: u64) -> mp_protocol::EventEnvelope {
    let status = mp_protocol::send::HoldStatus {
        operation_id: "op-old-daemon".to_string(),
        account: ACCOUNT.to_string(),
        draft_id: "d".to_string(),
        subject: "s".to_string(),
        hold_secs: 20,
        remaining_secs: 20,
        fires_at: "2026-09-11T08:00:20Z".to_string(),
        origin: "tui".to_string(),
    };
    mp_protocol::EventEnvelope {
        instance_id: instance_id.to_string(),
        revision,
        kind: mp_protocol::events::KIND_SEND_HOLD_STARTED.to_string(),
        payload: serde_json::to_value(status).expect("a HoldStatus serialises"),
    }
}

/// A snapshot of [`snapshot`]'s shape from another daemon instance.
fn restarted_snapshot(revision: u64) -> Bootstrap {
    Bootstrap {
        instance_id: "restarted".to_string(),
        ..snapshot(revision, 5)
    }
}

/// A resync against a daemon that restarted drops the holds the old one
/// announced, so `u` is the Message-context toggle-read again.
///
/// Before the fix `app.hold` outlived the daemon whose timer it described:
/// every `u` was caught as `CancelHeldSend` for an operation the new daemon
/// never saw, and toggle-read was dead for the rest of the session.
#[test]
fn a_restart_resync_drops_the_old_daemons_hold_and_frees_u() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let mut app = App::from_bootstrap(config(), &snapshot(1, 5));
    app.apply_event(&hold_started("resync", 2));
    assert!(app.hold.is_some(), "the hold landed");

    app.apply_resync_bootstrap(&restarted_snapshot(1));

    assert!(app.hold.is_none(), "the old daemon's hold died with it");
    assert!(app.holds.is_empty());

    let entry = crate::app::EmailEntry {
        msg: Some(crate::app::MessageRef::new(1)),
        draft_id: None,
        skip: None,
        selector: None,
        from: "a@example.com".to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: "s".to_string(),
        status: "inbox".to_string(),
        date_display: "2026-07-01".to_string(),
        date_sort: "2026-07-01T00:00:00".to_string(),
        has_attachments: false,
        read: false,
        answered: false,
        forwarded: false,
        flagged: false,
        is_invite: false,
    };
    app.emails = Arc::new(vec![entry]);
    app.visible = vec![0];
    app.list_index = 0;
    app.focus = crate::app::Focus::List;
    app.pending_actions.clear();

    app.handle_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('u'),
    ));

    assert!(
        app.pending_actions
            .iter()
            .any(|action| matches!(action, Action::ToggleRead)),
        "`u` toggles read again, got {:?}",
        app.pending_actions
    );
    assert!(!app
        .pending_actions
        .iter()
        .any(|action| matches!(action, Action::CancelHeldSend)));
}

/// A door for a restarted daemon: its bootstrap, and one hold it armed for
/// another window since the restart.
struct RestartDoor {
    calls: RefCell<Vec<String>>,
}

impl Queries for RestartDoor {
    fn call(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
        self.calls.borrow_mut().push(method.to_string());
        match method {
            "state.bootstrap" => Ok(serde_json::to_value(restarted_snapshot(3))?),
            "send.hold_status" => Ok(serde_json::json!({"holds": [{
                "operation_id": "op-new-daemon",
                "account": ACCOUNT,
                "draft_id": "d2",
                "subject": "s2",
                "hold_secs": 20,
                "remaining_secs": 12,
                "fires_at": "2026-09-11T08:01:00Z",
                "origin": "gui",
            }]})),
            other => anyhow::bail!("the restart door answers no {other}"),
        }
    }
}

/// Reconnecting to a restarted daemon takes that daemon's holds in place of
/// the old one's, through `send.hold_status`.
#[test]
fn a_reconnect_to_a_restarted_daemon_takes_its_holds() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let mut app = App::from_bootstrap(config(), &snapshot(1, 5));
    app.apply_event(&hold_started("resync", 2));

    let door = RestartDoor {
        calls: RefCell::new(Vec::new()),
    };
    let (sender, events) = mpsc::channel();
    sender
        .send(Incoming::Reconnected {
            instance_id: "restarted".to_string(),
        })
        .expect("the drain has not dropped the stream");
    drain(&mut app, &door, &events);

    assert_eq!(
        door.calls.borrow().as_slice(),
        ["state.bootstrap".to_string(), "send.hold_status".to_string()]
    );
    assert_eq!(
        app.hold.as_ref().map(|hold| hold.operation_id.as_str()),
        Some("op-new-daemon"),
        "the new daemon's hold replaced the old one's"
    );
    assert_eq!(app.holds.len(), 1);
}
