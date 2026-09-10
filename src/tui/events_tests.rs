//! Events replace watcher threads: the contract (P5-U7, ticket #0124).
//!
//! This file is a **contract test**: it is written before the TUI can read the
//! daemon's event stream, against the surface P5-U8 has to supply
//! (`.agents/workflow/native-gui-daemon/plan.md` section 3.7, P5-U7/P5-U8), the
//! event semantics of `docs/daemon-protocol.md` ("Event semantics", "Long-running
//! operations") and the two follow-ups P5-U6 recorded: the 100 ms
//! `operation.status` poll, and `new_inbox_mail`'s place in a per-client answer
//! rather than on the stream. It does not compile against today's tree, which
//! has no `crate::tui::events`; that failure *is* the proof the contract has no
//! stub behind it. The implementer does not edit this file, they make it pass.
//!
//! # Contract
//!
//! ```text
//! mailypoppins::tui::events                                        // the module
//!
//! enum events::Incoming: Debug {
//!     Event(mp_protocol::EventEnvelope),               // one decoded `state.event`
//!     Resync { instance_id: String, reason: String },  // `state.resync_required`
//!     Disconnected { reason: String },                 // the daemon went away
//!     Reconnected { instance_id: String },             // a daemon answers again
//! }
//! type events::Subscription = std::sync::mpsc::Receiver<events::Incoming>
//!
//! enum events::Applied: Debug + PartialEq {
//!     Rows,                                    // the open list moved
//!     Counts,                                  // a sidebar count moved
//!     NewMail(Vec<mp_protocol::events::Arrival>),  // a tick, and what it notified about
//!     Operation(String),                       // the operation that finished, by id
//!     Duplicate,                               // at or below the watermark
//!     Refused,                                 // an unfamiliar instance
//!     Ignored,                                 // nothing this client reads
//! }
//!
//! fn events::drain(
//!     app: &mut App,
//!     door: &dyn crate::tui::queries::Queries,
//!     events: &events::Subscription,
//! ) -> usize
//!
//! impl App  { pub fn apply_event(&mut self, event: &mp_protocol::EventEnvelope) -> events::Applied }
//! impl Session { pub fn events(&mut self) -> Option<events::Subscription> }
//!
//! mp_protocol::events::Arrival { from: String, subject: String }   // Debug + PartialEq + Serde
//! mp_protocol::events::SyncCompleted::new_inbox_mail: Vec<Arrival>
//! ```
//!
//! Eight names, and nothing else. Every other type below is the TUI's own, the
//! daemon's own or the protocol's own and exists today.
//!
//! ## Why a `Receiver` and not a type of its own
//!
//! `Subscription` is a type alias over `std::sync::mpsc::Receiver`, which is
//! what `run_loop` already holds three of (`watch_rx`, `bg_rx`, `boot_rx`) and
//! what a `try_recv` drain reads without a second vocabulary. It also lets a
//! test feed the drain without a socket, which is what every row in section (d)
//! does: the stream under test is a channel, and whether the far end is a
//! session thread or a `Sender` in a test is not a property the drain may
//! observe.
//!
//! ## Why the connection's own state travels on that channel
//!
//! A daemon killed mid-session and a daemon restarted afterwards are facts the
//! UI has to show, and they arrive at the same place as the events do: the
//! session thread. Two more variants on one stream keep them ordered against
//! the events around them, where a second channel would let a `Reconnected`
//! overtake the last event of the dead instance and make the watermark
//! arithmetic disagree with what the client actually saw.
//!
//! ## Why `apply_event` is on `App` and `drain` is not
//!
//! Applying one event is the model's business and nothing else's, which is what
//! makes twelve of the rows below one call and one assertion. Draining is the
//! loop's: it is bounded, it re-bootstraps, and it therefore needs a door. The
//! door is an argument for the reason P5-U5 gave for `commands::dispatch` -
//! `drain` needs `&mut App` and a borrow of `app.session` inside it would
//! collide - and `Session::handle()` already exists for exactly that.
//!
//! ## Why the drain's bound is not a new constant
//!
//! `MAX_COALESCED_EVENTS` and `COALESCE_BUDGET` are the pre-draw drain's bounds
//! and P5-U5 pinned them unchanged; an event batch is drained in the same
//! pre-draw pass and is held to the same two. This file is a child module of
//! `src/tui/mod.rs` so it reads the constants themselves rather than a copy,
//! which is also why it lives beside the event loop rather than under `app/`.
//!
//! ## What the daemon owes, and where it is pinned
//!
//! Three things, and only the third is a Rust name:
//!
//! - **Account runtimes are on by default.** `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`
//!   and `lifecycle::ACCOUNT_RUNTIMES_ENV` are gone, which is the scan in
//!   section (e); that a daemon started *without* the variable serves a ready
//!   account is `tests/tui_daemon_recovery.rs`, over a real socket, because it
//!   is a property of `mp daemon run` and not of a dispatcher.
//! - **The watcher lives in the runtime.** `imap_watch`, `watcher_loop` and
//!   `graph_watcher_loop` leave `src/tui/`, which is the other scan in section
//!   (e); that a runtime's tick reaches a subscribed client as an event is
//!   `tests/tui_daemon_recovery.rs` again, through the fake sync outcome hook.
//! - **A tick carries its arrivals.** `SyncCompleted` gains `new_inbox_mail`,
//!   so the desktop notification of #0009 survives the watcher that used to
//!   feed it. P5-U6 put the arrival list beside the *answer* to the client that
//!   asked for a pass, and said so: *"that is P5-U8's to fix when the
//!   notification moves onto the event stream"*. Nobody asks for a runtime's
//!   tick, so the answer has no reader and the event is the only carrier left.
//!
//! ## Determinism
//!
//! Every row owns a per-thread data root ([`crate::config::test_env::TestDataDir`],
//! #0077) held alive for as long as the fixture, so the store, the drafts
//! directory and the daemon's runtime directory resolve under it and no row can
//! reach the developer's tree. Rows are ingested through the real
//! [`crate::ingest::ingest_message`], dates are frozen literals, and nothing
//! renders, so no theme is pinned and no snapshot is minted. No row sleeps and
//! no row polls: an event is put on a channel and the drain is called.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use serde_json::{json, Value};

use mp_protocol::events::{Arrival, SyncCompleted};
use mp_protocol::state::Bootstrap;
use mp_protocol::{EventEnvelope, Request, RequestId, JSONRPC_VERSION};

use crate::config::{AccountConfig, GlobalConfig, ImapConfig};
use crate::daemon::config::{ConfigState, ConfigStore};
use crate::daemon::dispatch::{ClientCtx, ClientKind};
use crate::daemon::runtime::InstanceMeta;
use crate::daemon::server::DaemonState;
use crate::parse::FetchedEmail;
use crate::tui::app::{build_mailboxes, Action, App, MailboxKind};
use crate::tui::commands::dispatch;
use crate::tui::events::{drain, Applied, Incoming, Subscription};
use crate::tui::queries::{list_emails, Queries};
use crate::tui::test_daemon::TestDaemon;

use super::{COALESCE_BUDGET, MAX_COALESCED_EVENTS};

/// The one account every fixture configures, and the one every event names.
const ACCOUNT: &str = "alice";

/// The instance id the fixture daemon publishes under, which is also the one
/// its `state.bootstrap` reports.
const INSTANCE: &str = "p5u7-events";

/// The revision a row's first bootstrap watermarks at. Any number above the
/// daemon's own start would do; it is a literal so a row that asserts
/// `Duplicate` names the number it is a duplicate of.
const WATERMARK: u64 = 10;

// ---------------------------------------------------------------------------
// (a) one event, one model change
// ---------------------------------------------------------------------------

/// A `message.row` for the mailbox on screen lands in the list where a fresh
/// listing would have put it, without a second `message.list`.
///
/// The delta half of this is P5-U4's and is already tested in `bg.rs`; what is
/// new is that an *event* reaches it at all. Until P5-U8 the stream is
/// connected and drained by nobody (P5-U2), so this is the first row anywhere
/// that starts at an `EventEnvelope`.
#[test]
fn a_row_event_lands_in_the_open_list() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);
    assert_eq!(app.emails.len(), 3, "the fixture seeds three inbox rows");

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "message.row",
        json!({
            "account": ACCOUNT,
            "mailbox": "inbox",
            "message": wire_row(4, 4, "d", "2024-01-01T12:00:00"),
        }),
    ));

    assert_eq!(applied, Applied::Rows);
    assert_eq!(app.emails.len(), 4, "the arrival is in the list");
    assert_eq!(
        app.emails[0].subject, "d",
        "the newest row goes where the store's order would have put it"
    );
}

/// A count change is a `state.invalidate` over `mailbox:<account>/<slug>`
/// scoped to `counts`, and it moves the sidebar and not the list.
///
/// The scope is what keeps the two apart: `MessageRowDelta::decode` returns
/// `None` for it on purpose (P5-U4), because refetching the open list on every
/// count change would put back the per-event whole-list transfer the deltas
/// exist to avoid.
#[test]
fn a_count_invalidate_moves_the_sidebar_and_not_the_list() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.session = Some(TestDaemon::new(&[ACCOUNT]).session());
    app.mailbox_counts = vec![0; app.mailboxes.len()];
    watermarked(&mut app);
    let inbox = app.active_mailbox;
    let listed = app.emails.len();

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "state.invalidate",
        json!({
            "resource": format!("mailbox:{ACCOUNT}/inbox"),
            "scope": {"query": "counts"},
        }),
    ));

    assert_eq!(applied, Applied::Counts);
    assert_eq!(
        app.mailbox_counts[inbox], 3,
        "the count came back from the daemon, not from the stale sidebar"
    );
    assert_eq!(
        app.emails.len(),
        listed,
        "a count change is not a listing change"
    );
}

/// An event the snapshot already carries is dropped without a word.
///
/// The daemon queues events from the moment a connection registers, which is
/// *before* it captures the snapshot (`docs/daemon-protocol.md`, "Delivery,
/// coalescing and caps"), so a revision at or below the watermark is the normal
/// case and not a fault. Applying one would double a row.
#[test]
fn a_duplicate_revision_is_dropped_without_a_word() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK,
        "message.row",
        json!({
            "account": ACCOUNT,
            "mailbox": "inbox",
            "message": wire_row(4, 4, "d", "2024-01-01T12:00:00"),
        }),
    ));

    assert_eq!(applied, Applied::Duplicate);
    assert_eq!(
        app.emails.len(),
        3,
        "a revision the snapshot already carries changes nothing"
    );
}

/// An event from a daemon this client never bootstrapped against is refused
/// whatever its number, because revisions are only comparable within the
/// instance that issued them.
///
/// This is the half of the recovery story that a restarted daemon exercises:
/// a client that kept applying past an instance change would build a state
/// nothing on the daemon's side corresponds to.
#[test]
fn an_event_from_another_instance_is_refused() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);

    let applied = app.apply_event(&envelope(
        "a-daemon-that-restarted",
        WATERMARK + 1,
        "message.row",
        json!({
            "account": ACCOUNT,
            "mailbox": "inbox",
            "message": wire_row(4, 4, "d", "2024-01-01T12:00:00"),
        }),
    ));

    assert_eq!(applied, Applied::Refused);
    assert_eq!(
        app.emails.len(),
        3,
        "nothing from an unfamiliar instance applies"
    );
    assert_eq!(
        app.apply_event(&envelope(
            INSTANCE,
            WATERMARK + 1,
            "message.row",
            json!({
                "account": ACCOUNT,
                "mailbox": "inbox",
                "message": wire_row(4, 4, "d", "2024-01-01T12:00:00"),
            })
        )),
        Applied::Refused,
        "the refusal is sticky: only a fresh bootstrap clears it"
    );
}

/// A kind the TUI has no handler for is ignored rather than guessed at.
///
/// `signature.changed` is a real kind of this protocol version that no TUI
/// surface reads; a client that treated an unknown kind as a reason to reload
/// would turn every future protocol addition into a refetch storm.
#[test]
fn a_kind_the_tui_does_not_read_is_ignored() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "signature.changed",
        json!({"name": "work", "path": "/tmp/work.md"}),
    ));

    assert_eq!(applied, Applied::Ignored);
    assert!(
        app.pending_actions.is_empty(),
        "an ignored kind reloads nothing"
    );
}

// ---------------------------------------------------------------------------
// (b) the watcher's work, as events
// ---------------------------------------------------------------------------

/// A tick that ingested inbox mail notifies the user and refreshes the list.
///
/// This is what `imap_watch` plus `BgResult::Fetch` did between them: the
/// watcher noticed, a quick sync ran, and its result carried both the status
/// line and the arrivals (#0009). The runtime does the first two now, and the
/// arrivals ride the tick's own event because nobody asked for the pass and
/// there is therefore no answer for them to ride.
#[test]
fn a_tick_with_arrivals_notifies_the_user_and_refreshes_the_list() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.global_config.notifications = true;
    watermarked(&mut app);

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "sync.completed",
        tick(
            2,
            0,
            &[("Ada <ada@example.com>", "Analytical engine")],
            None,
        ),
    ));

    assert_eq!(
        applied,
        Applied::NewMail(vec![Arrival {
            from: "Ada <ada@example.com>".to_string(),
            subject: "Analytical engine".to_string(),
        }]),
        "the arrivals the tick reported are the ones the notification renders"
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Synced: 2 new, 0 existing"),
        "one wording, from mp_client::format::sync_status_line and nowhere else"
    );
    assert!(
        app.pending_actions
            .iter()
            .any(|a| matches!(a, Action::LoadMailbox { .. })),
        "a tick that wrote rows reloads the open mailbox off the UI thread"
    );
}

/// `notifications = false` in `config.toml` notifies nobody, and the tick still
/// lands.
///
/// The opt-in is #0009's and it is read where it was always read, on the way to
/// the notifier rather than at the stream: an event this client dropped for a
/// setting would also drop the status line and the reload with it.
#[test]
fn notifications_off_notifies_nobody_and_still_syncs_the_view() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.global_config.notifications = false;
    watermarked(&mut app);

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "sync.completed",
        tick(
            2,
            0,
            &[("Ada <ada@example.com>", "Analytical engine")],
            None,
        ),
    ));

    assert_eq!(
        applied,
        Applied::NewMail(Vec::new()),
        "nothing was notified about, which is what the empty list says"
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Synced: 2 new, 0 existing"),
        "the line is not the notification and does not travel with it"
    );
}

/// A failed tick marks the account it belongs to, not the shared status line.
///
/// #0071, and it is the assertion that says the event carries the account:
/// `BgResult::Fetch` carried an index and the tick carries a name, and an
/// implementation that dropped the name on the floor would leave a
/// multi-account outage invisible exactly as it was before #0068.
#[test]
fn a_failed_tick_marks_the_account_it_belongs_to() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);

    app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "sync.completed",
        tick(0, 0, &[], Some("IMAP login failed: no such user")),
    ));

    assert!(
        app.accounts[0].sync_health.is_failed(),
        "the account carries its own outcome"
    );
    assert_eq!(
        app.accounts[0].sync_health.failure_lines().unwrap().1,
        "IMAP login failed: no such user"
    );
}

// ---------------------------------------------------------------------------
// (c) an operation finishes by event, not by poll
// ---------------------------------------------------------------------------

/// A quick sync starts its operation and asks nothing else.
///
/// P5-U6 gave each sync arm a worker thread reading `operation.status` every
/// 100 ms and recorded the replacement as this unit's: *"P5-U8 replaces the
/// poll with the `operation.finished` subscription"*. The call count is the
/// assertion, because a poll that had merely been made slower would still pass
/// a test that only looked at the first call.
#[test]
fn a_quick_sync_starts_an_operation_and_polls_nothing() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.imap_config = Some(imap_config());
    watermarked(&mut app);
    fixture.forget();

    assert!(
        dispatch(&mut app, &fixture, &Action::Fetch),
        "a quick sync is daemon-routed and handled"
    );

    assert_eq!(
        fixture.methods(),
        vec!["sync.quick".to_string()],
        "the arm starts the operation and returns; the finish arrives as an event"
    );
}

/// The finished operation lands where the poll's answer landed: on the status
/// line, with the arrivals it carried.
///
/// The id is read off the daemon's own answer rather than invented, so the row
/// pins the correspondence and not a string: an implementation that ignored the
/// id and applied every `operation.finished` it saw fails the next row.
#[test]
fn the_finished_operation_lands_where_the_poll_landed() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.imap_config = Some(imap_config());
    app.global_config.notifications = true;
    watermarked(&mut app);
    fixture.forget();
    dispatch(&mut app, &fixture, &Action::Fetch);
    let id = fixture.operation_id("sync.quick");

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "operation.finished",
        json!({
            "operation_id": id,
            "state": "succeeded",
            "result": {
                "blocked": false,
                "outcome": tick(3, 4, &[], None),
                "new_inbox_mail": [{"from": "Ada <ada@example.com>", "subject": "Engine"}],
            },
        }),
    ));

    assert_eq!(applied, Applied::Operation(id));
    assert_eq!(
        app.status_message.as_deref(),
        Some("Synced: 3 new, 4 existing"),
        "the same line the polled answer produced, from the same pure function"
    );
}

/// An `operation.finished` for work this client never started is ignored.
///
/// Operations are daemon-wide and their events reach every bootstrapped
/// connection (`docs/daemon-protocol.md`), so a TUI beside a `mp sync` sees the
/// other window's operations finish. Presenting one would put a line on the
/// screen about a pass the user did not ask this client for.
#[test]
fn an_operation_this_client_never_started_is_ignored() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);
    app.set_status("Ready".to_string());

    let applied = app.apply_event(&envelope(
        INSTANCE,
        WATERMARK + 1,
        "operation.finished",
        json!({
            "operation_id": "an-id-from-the-cli-window",
            "state": "succeeded",
            "result": {"blocked": false, "outcome": tick(9, 9, &[], None)},
        }),
    ));

    assert_eq!(applied, Applied::Ignored);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Ready"),
        "another client's operation does not write on this one's status line"
    );
}

// ---------------------------------------------------------------------------
// (d) the pre-draw drain
// ---------------------------------------------------------------------------

/// An empty stream costs one `try_recv` and nothing else.
///
/// The drain runs on every loop iteration, four times a second at the idle
/// tick, so "nothing happened" has to be free: a drain that called the daemon
/// to find out would be a query per frame.
#[test]
fn an_empty_stream_costs_nothing() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);
    fixture.forget();
    let (_sender, events) = stream();

    assert_eq!(drain(&mut app, &fixture, &events), 0);
    assert!(
        fixture.methods().is_empty(),
        "an idle frame asks the daemon nothing"
    );
}

/// The drain is bounded by the batch cap, and the rest waits for the next
/// frame.
///
/// The same bound the terminal-event drain of #0108 keeps, read here from the
/// constant itself rather than from a copy of its value: a flood of events -
/// a first sync of a large mailbox publishing a row per message - may not
/// starve the paint any more than a bracketed paste may.
#[test]
fn the_drain_is_bounded_by_the_batch_cap() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);
    let (sender, events) = stream();
    let flood = MAX_COALESCED_EVENTS + 10;
    for n in 0..flood {
        sender
            .send(Incoming::Event(envelope(
                INSTANCE,
                WATERMARK + 1 + n as u64,
                "signature.changed",
                json!({"name": format!("s{n}"), "path": "/tmp/s.md"}),
            )))
            .expect("the drain has not dropped the stream");
    }

    assert_eq!(
        drain(&mut app, &fixture, &events),
        MAX_COALESCED_EVENTS,
        "one frame drains at most one batch"
    );
    assert_eq!(
        drain(&mut app, &fixture, &events),
        flood - MAX_COALESCED_EVENTS,
        "the rest is still there, in order, for the next frame"
    );
    assert!(
        COALESCE_BUDGET >= std::time::Duration::from_millis(1),
        "the wall-clock half of the bound is the same one, and is not zero"
    );
}

/// `state.resync_required` costs a fresh `state.bootstrap` and nothing less.
///
/// The daemon says so itself rather than leaving a hole to infer: a poisoned
/// queue accepts no further domain event until the client bootstraps again, so
/// a client that only cleared a flag would sit in front of a state that stopped
/// moving.
#[test]
fn a_resync_notification_costs_a_fresh_bootstrap() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);
    fixture.forget();
    let (sender, events) = stream();
    sender
        .send(Incoming::Resync {
            instance_id: INSTANCE.to_string(),
            reason: "event_queue_overflow".to_string(),
        })
        .unwrap();

    assert_eq!(drain(&mut app, &fixture, &events), 1);

    assert_eq!(
        fixture.methods(),
        vec!["state.bootstrap".to_string()],
        "the one answer to a poisoned stream"
    );
}

/// A reconnect to a new daemon re-bootstraps, and the new instance's revisions
/// are accepted only afterwards.
///
/// This is the recovery contract in one process: the socket-level half, with a
/// real daemon killed and restarted, is `tests/tui_daemon_recovery.rs`. The
/// watermark and the instance both come from the bootstrap's answer rather than
/// from the reconnect notice, because the notice is this client's guess and the
/// answer is the daemon's word.
#[test]
fn a_reconnect_re_bootstraps_before_it_accepts_the_new_instance() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.apply_bootstrap(&bootstrap_at("the-daemon-that-died", WATERMARK));
    let after = fixture.bootstrap_revision() + 1;
    fixture.forget();
    let (sender, events) = stream();

    // Before the reconnect, the live daemon's own events are meaningless.
    assert_eq!(
        app.apply_event(&envelope(
            INSTANCE,
            after,
            "message.row",
            json!({
                "account": ACCOUNT,
                "mailbox": "inbox",
                "message": wire_row(4, 4, "d", "2024-01-01T12:00:00"),
            })
        )),
        Applied::Refused
    );

    sender
        .send(Incoming::Reconnected {
            instance_id: INSTANCE.to_string(),
        })
        .unwrap();
    assert_eq!(drain(&mut app, &fixture, &events), 1);
    assert_eq!(
        fixture.methods(),
        vec!["state.bootstrap".to_string()],
        "a reconnect is a bootstrap, not a resumed watermark"
    );

    assert_eq!(
        app.apply_event(&envelope(
            INSTANCE,
            after,
            "message.row",
            json!({
                "account": ACCOUNT,
                "mailbox": "inbox",
                "message": wire_row(4, 4, "d", "2024-01-01T12:00:00"),
            })
        )),
        Applied::Rows,
        "the new instance's revisions apply once a bootstrap has admitted them"
    );
}

/// A disconnect is shown and calls nobody.
///
/// `watcher_active` is the sidebar indicator the IMAP watcher used to own
/// (`src/tui/mod.rs`, `WatchEvent::Error` / `WatchEvent::Reconnected`); the
/// daemon session owns it now, and it is the one field a frame reads to say
/// whether this client is being told about new mail at all. Calling the daemon
/// on the way down is the direct fallback P5-U7 forbids, expressed as a call
/// count.
#[test]
fn a_disconnect_is_shown_and_calls_nobody() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    watermarked(&mut app);
    app.watcher_active = true;
    fixture.forget();
    let (sender, events) = stream();
    sender
        .send(Incoming::Disconnected {
            reason: "the daemon closed the connection".to_string(),
        })
        .unwrap();

    assert_eq!(drain(&mut app, &fixture, &events), 1);

    assert!(
        !app.watcher_active,
        "a client with no session is not being told about new mail"
    );
    assert!(
        fixture.methods().is_empty(),
        "a dead session is not a reason to open a store or to call anything else"
    );
}

// ---------------------------------------------------------------------------
// (e) the residue gates
// ---------------------------------------------------------------------------

/// The two watcher threads are gone from `src/tui/`.
///
/// `watcher_loop` (IMAP IDLE, `src/tui/helpers.rs`) and `graph_watcher_loop`
/// (the 60 s enumeration beside it) are the two threads this unit moves into
/// the daemon's account runtime, and `WatchEvent` is the channel `run_loop`
/// read them over. A scan rather than a behavioural assertion because what is
/// being pinned is an absence, and an absence has no call site to observe.
#[test]
fn the_watcher_threads_are_gone_from_the_tui() {
    assert_eq!(
        needles_under("src/tui", &WATCHER_NEEDLES),
        BTreeSet::new(),
        "the TUI still watches a server itself; the runtime does that now"
    );
}

/// The TUI never asks for an operation's status.
///
/// The behavioural half is `a_quick_sync_starts_an_operation_and_polls_nothing`,
/// which pins the one arm a test can drive; this pins the other three
/// (`sync.full`, `send.approved`, `calendar.rsvp`) and every arm a later unit
/// adds, because they all go through one helper and the string appears once.
#[test]
fn the_tui_never_asks_for_an_operations_status() {
    assert_eq!(
        needles_under("src/tui", &["\"operation.status\""]),
        BTreeSet::new(),
        "an operation finishes by event now, so nothing polls it"
    );
}

/// The account-runtimes opt-in is gone from the tree.
///
/// Plan section 3.7: *"turns on account runtimes by default (drops
/// `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`)"*. Both spellings are scanned, the
/// literal and the constant that names it, because a daemon that kept the
/// constant and defaulted it to true would leave a second switch nobody
/// documents.
#[test]
fn the_account_runtimes_opt_in_is_gone() {
    assert_eq!(
        needles_under("src", &RUNTIME_OPT_IN_NEEDLES),
        BTreeSet::new(),
        "runtimes are on by default, so the variable that turned them on is not \
         a variable any more"
    );
}

/// The scanner finds what it looks for, so a gate that passes says something.
///
/// The same guard `queries_tests.rs` and `actions_tests.rs` carry: a scan that
/// silently matched nothing - a renamed directory, a read that failed - would
/// turn all three gates above into green lines about nothing.
#[test]
fn the_source_scanner_finds_what_it_looks_for() {
    let found = needles_under("src/tui", &["fn handle_bg_result"]);
    assert!(
        found.contains(&(
            "src/tui/bg.rs".to_string(),
            "fn handle_bg_result".to_string()
        )),
        "the scanner did not find a symbol that is certainly there: {found:?}"
    );
    assert!(
        needles_under("src/tui", &["fn a_symbol_no_file_carries"]).is_empty(),
        "the scanner reports a symbol nothing declares"
    );
    assert!(
        needles_under("src/tui", &["MAX_COALESCED_EVENTS"])
            .iter()
            .all(|(file, _)| file != "src/tui/events_tests.rs"),
        "the scanner reads this file's own mentions as production code"
    );
}

/// The one compile-time assertion in this file: a [`Session`] hands out the
/// stream the drain reads.
///
/// Never called. `Session::events` cannot be exercised in process - it needs
/// the session thread and a socket, which is `tests/tui_daemon_recovery.rs` -
/// but without this line P5-U8 could satisfy every row above with a channel no
/// connection ever writes to, and the TUI would drain an empty stream for ever.
#[allow(dead_code)]
fn a_session_hands_out_the_event_stream(
    session: &mut crate::tui::session::Session,
) -> Option<Subscription> {
    session.events()
}

// ---------------------------------------------------------------------------
// The source scanner
// ---------------------------------------------------------------------------

/// What `src/tui/` may not contain once the watcher lives in the daemon.
const WATCHER_NEEDLES: [&str; 5] = [
    "watch_mailbox",
    "watcher_loop",
    "graph_watcher_loop",
    "WatchEvent",
    "GRAPH_POLL_SECS",
];

/// Both spellings of the pre-Phase-5 account-runtimes opt-in.
const RUNTIME_OPT_IN_NEEDLES: [&str; 2] = [
    "MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES",
    "ACCOUNT_RUNTIMES_ENV",
];

/// Every `(file, needle)` found in the production code below `relative`.
///
/// Production code: line comments (`//`, `///` and `//!` alike) and
/// `#[cfg(test)]` modules are removed first, so a doc comment that names a
/// symbol and a test module that builds one are not calls to it. This file is
/// skipped outright, because it names every needle it looks for.
fn needles_under(relative: &str, needles: &[&str]) -> BTreeSet<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeSet::new();
    for path in rust_files(&root.join(relative)) {
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if name == "src/tui/events_tests.rs" {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let source = strip_test_modules(&strip_line_comments(&source));
        for needle in needles {
            if source.contains(needle) {
                found.insert((name.clone(), (*needle).to_string()));
            }
        }
    }
    found
}

/// Every `.rs` file below `dir`, in a stable order.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Drop everything from `//` to the end of the line, string literals included:
/// a scan over source text is not a parser, and a doc comment naming
/// `watcher_loop` is not a thread.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop every `#[cfg(test)] mod … { … }` block, braces matched.
///
/// The same helper `tests/architecture_boundaries.rs`, `app/queries_tests.rs`
/// and `actions_tests.rs` use, with the same limitation: braces inside string
/// literals inside a test module would confuse it, and no scanned file has one.
fn strip_test_modules(source: &str) -> String {
    let mut out = source.to_string();
    loop {
        let Some(at) = out.find("#[cfg(test)]") else {
            return out;
        };
        let after = &out[at + "#[cfg(test)]".len()..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with("mod ") && !trimmed.starts_with("pub mod ") {
            out.replace_range(at..at + 1, " ");
            continue;
        }
        let Some(open) = out[at..].find('{').map(|i| at + i) else {
            return out;
        };
        let mut depth = 0usize;
        let mut end = out.len();
        for (i, ch) in out[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.replace_range(at..end, "");
    }
}

// ---------------------------------------------------------------------------
// The fixture: one seeded store, one in-process daemon over it, every call and
// every answer recorded
// ---------------------------------------------------------------------------

/// A daemon assembled over a fixture data root, reachable as a [`Queries`],
/// remembering what it was asked *and* what it answered.
///
/// The answer is what `actions_tests.rs`'s fixture does not keep and what this
/// file needs: an operation's id is minted by the daemon, and a row that pinned
/// the correspondence between a started operation and a finished one would
/// otherwise have to invent the id and pin nothing.
struct Fixture {
    daemon: DaemonState,
    runtime: tokio::runtime::Runtime,
    calls: RefCell<Vec<(String, Value, Value)>>,
    _data: crate::config::test_env::TestDataDir,
}

impl Fixture {
    /// A daemon over a fresh data root with an empty store for [`ACCOUNT`].
    ///
    /// The store is created before the daemon is assembled because
    /// `account::state_of` probes the store *file*: an account with no file is
    /// `blocked`, and the message methods refuse a blocked account.
    fn new() -> Fixture {
        let data = crate::config::test_env::TestDataDir::new();
        let root = crate::config::mailypoppins_data_dir();
        std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
        drop(crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store"));

        let config = Arc::new(ConfigStore::new(
            root.join("config.toml"),
            ConfigState::Ok,
            global_config(),
            false,
        ));
        let daemon = DaemonState::new(
            InstanceMeta {
                app_version: "0.0.0-p5u7".to_string(),
                protocol_min: mp_protocol::PROTOCOL_MIN,
                protocol_max: mp_protocol::PROTOCOL_MAX,
                instance_id: INSTANCE.to_string(),
                pid: 42,
                started_at: "2026-07-28T09:00:00Z".to_string(),
                data_dir: root.clone(),
                config_dir: root.clone(),
            },
            config,
        );
        Fixture {
            daemon,
            runtime: {
                // Every thread this runtime starts is pointed at the fixture's
                // data root: the override of #0077 is thread-local and an
                // operation's body runs on a task of its own. Multi-threaded
                // for the reason `test_daemon.rs` records - a current-thread
                // runtime only drives a spawned task while something is inside
                // `block_on`, and an operation's caller never is.
                let root = crate::config::mailypoppins_data_dir();
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .on_thread_start(move || {
                        std::mem::forget(crate::config::test_env::DataDirOverride::set(&root));
                    })
                    .build()
                    .expect("a multi-thread runtime")
            },
            calls: RefCell::new(Vec::new()),
            _data: data,
        }
    }

    /// The TUI's own connection context: a `tui` client at protocol 1.
    fn ctx() -> ClientCtx {
        ClientCtx {
            connection_id: 1,
            kind: ClientKind::Tui,
            protocol: 1,
            capabilities: Vec::new(),
        }
    }

    /// Every method asked for so far, in order.
    fn methods(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .map(|(method, _, _)| method.clone())
            .collect()
    }

    /// Forget what has been asked so far, so a row asserts about its own event
    /// and not about the listing that seeded it.
    fn forget(&self) {
        self.calls.borrow_mut().clear();
    }

    /// The operation id the daemon minted for the one call to `method`.
    fn operation_id(&self, method: &str) -> String {
        let calls = self.calls.borrow();
        let answers: Vec<&Value> = calls
            .iter()
            .filter(|(name, _, _)| name == method)
            .map(|(_, _, result)| result)
            .collect();
        assert_eq!(answers.len(), 1, "expected exactly one call to {method}");
        answers[0]["operation_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{method} answered no operation id: {}", answers[0]))
            .to_string()
    }

    /// The revision this daemon's `state.bootstrap` reports, without recording
    /// the call.
    fn bootstrap_revision(&self) -> u64 {
        let answer = self
            .call("state.bootstrap", json!({}))
            .expect("the fixture daemon bootstraps");
        self.forget();
        serde_json::from_value::<Bootstrap>(answer)
            .expect("a Bootstrap")
            .revision
    }
}

impl Queries for Fixture {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(RequestId::Num(1)),
            method: method.to_string(),
            params: params.clone(),
        };
        let outcome = self
            .runtime
            .block_on(self.daemon.dispatcher.dispatch(&Fixture::ctx(), request))
            .map_err(|e| anyhow::anyhow!("{method}: {e}"))?;
        self.calls
            .borrow_mut()
            .push((method.to_string(), params, outcome.result.clone()));
        Ok(outcome.result)
    }
}

// ---------------------------------------------------------------------------
// Fixture data
// ---------------------------------------------------------------------------

/// The one-account configuration every fixture is built from.
///
/// The IMAP block is [`DISCARD_PORT`] on loopback and **nothing ever connects
/// to it**, the arrangement `tests/support/sync_fixture.rs` established for its
/// `gamma`: credential resolution refuses before a socket is opened, so the
/// account is not local-only - `sync.quick` mints an operation id for it rather
/// than refusing it outright - and no row here can reach a network.
fn global_config() -> GlobalConfig {
    GlobalConfig {
        accounts: vec![AccountConfig {
            name: ACCOUNT.to_string(),
            imap: crate::config::ImapSettings {
                host: "127.0.0.1".to_string(),
                port: DISCARD_PORT,
                username: ACCOUNT.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// The discard port, for the reason [`global_config`] gives.
const DISCARD_PORT: u16 = 9;

/// The resolved IMAP block an `App` carries, for the two rows whose action
/// checks for one before it calls (the verbatim "IMAP not configured" refusal
/// is client-side and stays there).
fn imap_config() -> ImapConfig {
    ImapConfig {
        host: "127.0.0.1".to_string(),
        port: DISCARD_PORT,
        username: ACCOUNT.to_string(),
        password: String::new(),
        accept_invalid_certs: false,
        auth_method: crate::config::AuthMethod::Password,
        fetch_concurrency: 4,
        body_fetch_deadline_secs: 30,
    }
}

/// One fixture message, everything about it derived from `subject` so a failure
/// names the row it is about.
fn fixture_email(subject: &str, date: &str, read: bool) -> FetchedEmail {
    FetchedEmail {
        from: format!("Sender {subject} <s@example.com>"),
        to: "me@example.com".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.into(),
        date: date.into(),
        body_text: format!("body of {subject}"),
        html_body: None,
        has_attachments: false,
        message_id: Some(format!("<{subject}@example.com>")),
        attachments: Vec::new(),
        flags: crate::types::MessageFlags::seen(read),
        calendar_ics: None,
        event: None,
    }
}

/// Write a fixture message through the real ingest API, so the rows under test
/// are the rows the sync path actually produces.
fn ingest_fixture(mailbox: &str, uid: i64, email: &FetchedEmail) {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    crate::ingest::ingest_message(
        &store,
        &blobs,
        &crate::ingest::IngestInput {
            account: ACCOUNT,
            mailbox,
            uid,
            email,
            raw: None,
        },
    )
    .unwrap();
}

/// Three inbox messages, oldest first, which is the list every row above holds.
fn seed_inbox() {
    for (uid, subject, hour) in [(1, "a", "09"), (2, "b", "10"), (3, "c", "11")] {
        ingest_fixture(
            "inbox",
            uid,
            &fixture_email(
                subject,
                &format!("Mon, 01 Jan 2024 {hour}:00:00 +0000"),
                true,
            ),
        );
    }
}

/// An `App` on [`ACCOUNT`]'s inbox, with the list the daemon lists, one
/// `AccountState` for that account and the cursor on the first row.
///
/// The rows come through [`list_emails`] rather than through the store, so the
/// uid index a remove delta needs is populated exactly as a real frame's is.
fn app_on_inbox(fixture: &Fixture) -> App {
    let mut app = App::default_for_tests();
    app.account_config = AccountConfig {
        name: ACCOUNT.to_string(),
        ..Default::default()
    };
    app.accounts = vec![account_state(ACCOUNT)];
    app.active_account = 0;
    app.mailboxes = build_mailboxes(&app.account_config);
    app.mailbox_counts = vec![0; app.mailboxes.len()];
    app.email_cache = vec![None; app.mailboxes.len()];
    app.active_mailbox = app
        .find_mailbox_by_kind(MailboxKind::Inbox)
        .expect("the Inbox mailbox is one of the four roles");
    app.emails = Arc::new(list_emails(fixture, ACCOUNT, "inbox").expect("the daemon lists inbox"));
    app.rebuild_visible();
    app.list_index = 0;
    app
}

/// A minimal `AccountState`, built as a struct literal rather than through
/// `AccountState::new`, which reads the user's config and keyring.
fn account_state(name: &str) -> crate::tui::app::AccountState {
    crate::tui::app::AccountState {
        account_config: AccountConfig {
            name: name.to_string(),
            ..Default::default()
        },
        imap_config: None,
        smtp_config: None,
        graph_config: None,
        signature_content: None,
        archive_server_name: "Archive".to_string(),
        drafts_dir: None,
        mailboxes: Vec::new(),
        mailbox_counts: vec![0],
        email_cache: vec![None],
        sidebar_index: 0,
        active_mailbox: 0,
        list_index: 0,
        cursor_ref: None,
        headers_scroll: 0,
        preview_scroll: 0,
        selection: std::collections::HashSet::new(),
        search_query: String::new(),
        watcher_active: false,
        opening: false,
        outbox: crate::outbox::OutboxCounts::default(),
        has_unseen: false,
        sync_health: crate::sync_health::SyncHealth::default(),
    }
}

/// Watermark `app` at [`WATERMARK`] against [`INSTANCE`], which is what a
/// bootstrap does and the only way a client may set one.
fn watermarked(app: &mut App) {
    app.apply_bootstrap(&bootstrap_at(INSTANCE, WATERMARK));
}

/// The smallest bootstrap that carries an instance and a revision: no account
/// section, so `apply_bootstrap` leaves the fixture's own accounts alone and
/// the row is about the watermark and nothing else.
fn bootstrap_at(instance: &str, revision: u64) -> Bootstrap {
    Bootstrap {
        instance_id: instance.to_string(),
        revision,
        ..Bootstrap::default()
    }
}

/// One event envelope, as the daemon frames it.
fn envelope(instance: &str, revision: u64, kind: &str, payload: Value) -> EventEnvelope {
    EventEnvelope {
        instance_id: instance.to_string(),
        revision,
        kind: kind.to_string(),
        payload,
    }
}

/// One `sync.completed` payload, with its arrivals.
///
/// Rendered through [`SyncCompleted`] itself rather than as a hand-written
/// object, so a row cannot pin a payload the daemon could not produce and the
/// new `new_inbox_mail` member travels the way every other member does.
fn tick(saved: u64, skipped: u64, arrivals: &[(&str, &str)], error: Option<&str>) -> Value {
    let outcome = SyncCompleted {
        account: ACCOUNT.to_string(),
        severity: if error.is_some() {
            mp_protocol::events::Severity::Error
        } else {
            mp_protocol::events::Severity::Ok
        },
        saved,
        skipped,
        flags_updated: 0,
        pruned: 0,
        prunes_deferred: 0,
        uid_rebound: 0,
        uidvalidity_resets: 0,
        bodies_truncated: 0,
        non_converging: Vec::new(),
        failed_mutations: 0,
        error: error.map(str::to_string),
        new_inbox_mail: arrivals
            .iter()
            .map(|(from, subject)| Arrival {
                from: (*from).to_string(),
                subject: (*subject).to_string(),
            })
            .collect(),
    };
    // Two readers: a `sync.completed` event's payload *is* the outcome, and an
    // `operation.finished` result carries the same object under `outcome`.
    serde_json::to_value(&outcome).expect("a SyncCompleted serialises")
}

/// One `message.list` row, as the wire carries it (P5-U4's seven added fields
/// included).
fn wire_row(id: i64, uid: i64, subject: &str, date_sort: &str) -> Value {
    json!({
        "id": id,
        "uid": uid,
        "message_id": format!("<{subject}@example.com>"),
        "from": format!("Sender {subject} <s@example.com>"),
        "to": "me@example.com",
        "cc": Value::Null,
        "reply_to": Value::Null,
        "bcc": Value::Null,
        "subject": subject,
        "date_sort": date_sort,
        "date_display": "Mon, 01 Jan 2024 12:00:00 +0000",
        "flags": {"seen": true, "answered": false, "forwarded": false, "flagged": false},
        "has_attachments": false,
        "is_invite": false,
    })
}

/// A stream the test writes and the drain reads, which is the shape the session
/// thread's end has too.
fn stream() -> (mpsc::Sender<Incoming>, Subscription) {
    mpsc::channel::<Incoming>()
}
