//! The daemon's diagnostics in the activity overlay: the contract (P6-U7,
//! ticket #0125).
//!
//! The socket half is `tests/daemon_diagnostics.rs`, which pins
//! `diagnostic.health`, `diagnostic.logs`, `diagnostic.log_path`,
//! `diagnostic.support_bundle` and the `diagnostic.check_changed` event. This
//! file pins the half the plan's sentence ends on - *"Feeds the TUI activity
//! overlay and the future GUI"* - because the ring the overlay renders, the
//! status line it must not disturb and the golden frames it must not move are
//! all in process.
//!
//! **No new Rust name.** The whole contract is behaviour over names that exist:
//! `App::apply_event`, `App::apply_bootstrap`, `App::status_log`,
//! `StatusLevel` and `Applied`. So this file compiles against the tree as
//! committed and its contract rows fail at runtime, exactly as
//! `tests/daemon_shutdown.rs` and `tests/daemon_service.rs` did. The
//! implementer (P6-U8) does not edit it; they make it pass.
//!
//! # The reading, and why this one
//!
//! ```text
//! state.event {kind: "diagnostic.check_changed", payload: {name, status, detail}}
//!     -> App::push_status(format!("{name}: {detail}"), level), Applied::Ignored
//!        level = Warning for `warn`, Error for `fail`, Success for `ok`
//!        status_message is NOT set: a check the user did not ask about may not
//!        overwrite the sentence his own last action put on the status line.
//!
//! state.bootstrap's snapshot.diagnostics
//!     -> one ring entry per item, in snapshot order, at the same levels,
//!        landed by App::apply_bootstrap, so a window that opens the overlay
//!        finds the daemon's current complaints already in it.
//! ```
//!
//! Two other readings were available and are refused here.
//!
//! - **Calling `diagnostic.logs` when the overlay opens** and merging the
//!   daemon's lines into the ring. It puts a socket round trip on the keypress
//!   path of the UI thread - `sl` would block for as long as the daemon takes
//!   to read a file - and it would make the ring's content depend on when the
//!   overlay was opened, which no golden frame could pin. The daemon's log is
//!   reachable as a whole through `mp daemon logs` and through `sf`
//!   (`Action::OpenLogFile`, `INT-02`), both of which open a file rather than
//!   paginate one into a ring of 200 lines.
//! - **A sixth `Applied` variant.** `apply_shutting_down` already records why
//!   there is none: the drain does not branch on this, nothing is reloaded and
//!   nothing is refetched, which is exactly what `Applied::Ignored` promises a
//!   caller. A variant would be a contract change for a log line.
//!
//! # Why no golden frame moves
//!
//! Nothing here touches `status_message`, `bg_count`, a mailbox count or a
//! list, so the only surface that can change is the activity pane and the
//! activity overlay, and both render `status_log`. Every golden frame is built
//! from a bootstrap whose `snapshot.diagnostics` is empty and applies no
//! `diagnostic.check_changed`, so every frame keeps the `No activity yet` line
//! it has today. The row `a_healthy_bootstrap_leaves_the_ring_empty` is that
//! statement as an assertion.

use mp_protocol::state::{Bootstrap, Snapshot};
use mp_protocol::EventEnvelope;
use serde_json::json;

use crate::app::{App, StatusLevel};
use crate::events::Applied;

/// The kind a flipped check travels as.
const KIND_CHECK_CHANGED: &str = "diagnostic.check_changed";

/// The instance every event travels under, and the one every fixture
/// bootstraps against.
const INSTANCE: &str = "p6u7-diagnostics";

/// The revision a fixture's bootstrap watermarks at.
const WATERMARK: u64 = 10;

// ---------------------------------------------------------------------------
// (a) One event, one ring entry
// ---------------------------------------------------------------------------

/// A check that went to `warn` is a warning in the ring, and nothing else
/// moved.
#[test]
fn a_warning_check_lands_in_the_activity_ring() {
    let mut app = bootstrapped(Vec::new());
    let before = app.status_message.clone();

    let applied = app.apply_event(&check_event("account:alpha", "warn", "beta holds the lock"));

    assert_eq!(
        applied,
        Applied::Ignored,
        "nothing is reloaded and nothing is refetched, which is what Ignored promises"
    );
    let entry = last_entry(&app);
    assert_eq!(
        entry.0, "account:alpha: beta holds the lock",
        "the line is the check's name and its sentence"
    );
    assert_eq!(entry.1, "Warning");
    assert_eq!(
        app.status_message, before,
        "a check the user did not ask about does not take the status line"
    );
}

/// A check that failed is an error in the ring.
#[test]
fn a_failing_check_lands_as_an_error() {
    let mut app = bootstrapped(Vec::new());

    app.apply_event(&check_event(
        "config_loaded",
        "fail",
        "expected `]` at line 1",
    ));

    let entry = last_entry(&app);
    assert_eq!(entry.0, "config_loaded: expected `]` at line 1");
    assert_eq!(entry.1, "Error");
}

/// A check that recovered says so, because a warning nobody saw cleared is a
/// warning the user keeps believing.
#[test]
fn a_recovered_check_lands_as_a_success() {
    let mut app = bootstrapped(Vec::new());

    app.apply_event(&check_event("config_loaded", "ok", "config.toml loaded"));

    let entry = last_entry(&app);
    assert_eq!(entry.0, "config_loaded: config.toml loaded");
    assert_eq!(entry.1, "Success");
}

/// Several flips are several entries, in arrival order, which is what the
/// overlay scrolls through.
#[test]
fn every_flip_is_its_own_entry_in_arrival_order() {
    let mut app = bootstrapped(Vec::new());

    app.apply_event(&check_event("config_loaded", "fail", "first"));
    app.apply_event(&check_event("store_open", "warn", "second"));
    app.apply_event(&check_event("config_loaded", "ok", "third"));

    assert_eq!(
        messages(&app),
        vec![
            "config_loaded: first".to_string(),
            "store_open: second".to_string(),
            "config_loaded: third".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// (b) The watermark rules apply, because this is an ordinary event
// ---------------------------------------------------------------------------

/// An event at or below the watermark is a duplicate the snapshot already
/// carried, and it writes nothing.
#[test]
fn a_duplicate_check_event_writes_nothing() {
    let mut app = bootstrapped(Vec::new());
    let mut event = check_event("store_open", "warn", "one store would not open");
    event.revision = WATERMARK;

    assert_eq!(app.apply_event(&event), Applied::Duplicate);
    assert!(
        app.status_log.is_empty(),
        "the bootstrap already carried this check: {:?}",
        messages(&app)
    );
}

/// An event from an instance this client never bootstrapped against is
/// refused, check or not.
#[test]
fn a_check_event_from_another_instance_is_refused() {
    let mut app = bootstrapped(Vec::new());
    let mut event = check_event("store_open", "fail", "another daemon's store");
    event.instance_id = "someone-else".to_string();

    assert_eq!(app.apply_event(&event), Applied::Refused);
    assert!(app.status_log.is_empty(), "and it wrote nothing");
}

// ---------------------------------------------------------------------------
// (c) The bootstrap seeds the ring
// ---------------------------------------------------------------------------

/// The snapshot's diagnostics are in the ring before the overlay is ever
/// opened, so `sl` on a freshly started window shows what the daemon is
/// unhappy about.
#[test]
fn the_bootstrap_seeds_the_ring_with_the_daemons_complaints() {
    let app = bootstrapped(vec![
        json!({"name": "account:beta", "status": "warn", "detail": "another engine holds the lock"}),
        json!({"name": "account:gamma", "status": "fail", "detail": "no local store yet"}),
    ]);

    assert_eq!(
        messages(&app),
        vec![
            "account:beta: another engine holds the lock".to_string(),
            "account:gamma: no local store yet".to_string(),
        ],
        "in snapshot order, which is the daemon's report order"
    );
    assert_eq!(
        levels(&app),
        vec!["Warning", "Error"],
        "and at the levels the statuses map to"
    );
}

/// A healthy daemon leaves the ring exactly as it is, which is what keeps
/// every golden frame on its `No activity yet` line.
#[test]
fn a_healthy_bootstrap_leaves_the_ring_empty() {
    let app = bootstrapped(Vec::new());
    assert!(
        app.status_log.is_empty(),
        "an empty diagnostics array writes nothing: {:?}",
        messages(&app)
    );
}

/// A diagnostics entry that is not a check is ignored rather than rendered as
/// a line of JSON: the snapshot's array is `Value`, and a future daemon may
/// put something else in it.
#[test]
fn a_diagnostic_that_is_not_a_check_is_skipped() {
    let app = bootstrapped(vec![json!({"something": "else"}), json!("a bare string")]);
    assert!(
        app.status_log.is_empty(),
        "neither entry is a check: {:?}",
        messages(&app)
    );
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// An `App` that has bootstrapped against [`INSTANCE`] at [`WATERMARK`],
/// carrying `diagnostics` in its snapshot.
fn bootstrapped(diagnostics: Vec<serde_json::Value>) -> App {
    let mut app = App::default_for_tests();
    app.apply_bootstrap(&Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision: WATERMARK,
        snapshot: Snapshot {
            diagnostics,
            ..Snapshot::default()
        },
        ..Bootstrap::default()
    });
    app
}

/// One `diagnostic.check_changed`, as the daemon frames it.
///
/// The revision walks forward by itself, because a row applies several events
/// in sequence and one at or below the watermark would be dropped.
fn check_event(name: &str, status: &str, detail: &str) -> EventEnvelope {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(WATERMARK + 1);
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision: NEXT.fetch_add(1, Ordering::Relaxed),
        kind: KIND_CHECK_CHANGED.to_string(),
        payload: json!({"name": name, "status": status, "detail": detail}),
    }
}

/// The ring's messages, in order.
fn messages(app: &App) -> Vec<String> {
    app.status_log
        .iter()
        .map(|entry| entry.message.clone())
        .collect()
}

/// The ring's levels, in order.
fn levels(app: &App) -> Vec<&'static str> {
    app.status_log
        .iter()
        .map(|entry| level_name(&entry.level))
        .collect()
}

/// The last entry's message and level, or a failure saying the ring is empty.
fn last_entry(app: &App) -> (String, &'static str) {
    let entry = app
        .status_log
        .back()
        .expect("the event pushed one entry into the activity ring");
    (entry.message.clone(), level_name(&entry.level))
}

/// One level as its own name.
///
/// `StatusLevel` derives neither `PartialEq` nor `Copy`, and a contract test
/// may not add a derive to a production type; naming the variants here is the
/// comparison without the edit.
fn level_name(level: &StatusLevel) -> &'static str {
    match level {
        StatusLevel::Success => "Success",
        StatusLevel::Error => "Error",
        StatusLevel::Warning => "Warning",
        StatusLevel::Info => "Info",
        StatusLevel::Progress => "Progress",
    }
}
