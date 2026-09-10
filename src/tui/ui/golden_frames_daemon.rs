//! Daemon-backed golden frames (P5-U1, ticket #0124).
//!
//! This file is a **contract test**: it is written before the TUI can be built
//! from a daemon at all, against the surface Phase 5 fixes
//! (`.agents/workflow/native-gui-daemon/plan.md` section 3.7, P5-U1/P5-U2) and
//! the `state.bootstrap` shape in `docs/daemon-protocol.md`. It does not
//! compile against today's tree, which has neither `mp_protocol::state` nor
//! `App::from_bootstrap`; that failure *is* the proof the contract has no stub
//! behind it. The implementer (P5-U2) does not edit this file, they make it
//! pass.
//!
//! # Surface under test
//!
//! ```text
//! mp_protocol::state::Bootstrap            // Deserialize, the whole state.bootstrap result
//! mailypoppins::tui::app::App::from_bootstrap(
//!     global_config: crate::config::GlobalConfig,
//!     bootstrap: &mp_protocol::state::Bootstrap,
//! ) -> App
//! ```
//!
//! Two names, deliberately. Everything else the fixture needs already exists:
//! [`crate::daemon::server::DaemonState`] assembles a daemon over a data root,
//! [`crate::daemon::state::CanonicalState::apply`] commits the changes a
//! runtime would publish, and
//! [`crate::daemon::dispatch::Dispatcher::dispatch`] serves `state.bootstrap`.
//! The fields of `Bootstrap` are not pinned here: this file decodes one and
//! hands it straight to `App::from_bootstrap`, so P5-U2 owns the field set and
//! only has to keep `serde_json::from_value::<Bootstrap>(result)` working
//! against the documented shape.
//!
//! # What `App::from_bootstrap` must do
//!
//! Pinned by the frames below rather than by a field assertion, which is the
//! point of a golden frame:
//!
//! - One `AccountState` per snapshot account, in the snapshot's order (which is
//!   `config.toml`'s), `active_account` at 0.
//! - `mailboxes` from the snapshot's mailbox rows for that account: `{role,
//!   slug, label}` becomes the [`MailboxInfo`](crate::tui::app::MailboxInfo)
//!   [`build_mailboxes`](crate::tui::app::build_mailboxes) gives that role, so
//!   the icon and the [`MailboxKind`](crate::tui::app::MailboxKind) a renderer
//!   branches on come from the role and the label comes from the wire.
//! - `mailbox_counts` from each row's `total`, in the same order.
//! - `opening` from the account's `state`: `opening` stays the #0003 loading
//!   state (zeroed counts and the `··` marker), `ready` clears it.
//! - No store opened, no engine lock taken, no mail directory walked, no
//!   message row invented: a bootstrap carries no messages, and the first paint
//!   must not wait for one (#0003, and the plan's "the shell still paints
//!   before any store opens").
//!
//! # The oracle
//!
//! Every frame asserts the daemon-built frame is **byte-identical to the
//! hand-built frame of the same fixture**, which is the stronger contract the
//! ticket offers: those 18 frames are already pinned by the reviewed snapshots
//! in `src/tui/ui/snapshots/`, so a daemon-built frame that matches one is
//! pinned by it too, and no second family of snapshot files has to be approved
//! on trust. The two frames that have no hand-built counterpart - an `opening`
//! account, and a bootstrap carrying one mailbox row more - carry a `…_daemon`
//! insta snapshot of their own, minted at P5-U2's first `cargo insta review`.
//!
//! [`super::golden_frames`] owns the frozen fixture and the capture helpers;
//! this module reuses both, so the two families cannot drift. Only the 18
//! frames have a daemon variant: `frames_are_reproducible` gets one below, and
//! `legend_tags_the_states_it_claims_to` is about the tag table rather than
//! about an `App`, so it has none.
//!
//! # Determinism
//!
//! The same three rules the hand-built frames keep, plus one:
//!
//! - The theme is pinned before the first `App` is built, not only at capture
//!   time, because `App::from_bootstrap` selects a theme the way `App::new`
//!   does and `theme::init` is a `OnceLock`.
//! - The data root is a tempdir for the calling thread only
//!   ([`crate::config::test_env::TestDataDir`]), held alive for as long as the
//!   `App` is: per-account paths (the drafts directory, the secrets file)
//!   resolve under it, so no frame can depend on the developer's own tree and
//!   no test can reach a real keyring.
//! - The daemon is seeded from a literal [`crate::config::GlobalConfig`] and
//!   reads no file: `state.bootstrap` is config-seeded in this build, and the
//!   counts it reports are committed as the
//!   [`Change::MailboxCounts`](crate::daemon::state::Change) a live runtime
//!   would publish. A store with messages in it is P5-U3's fixture, not this
//!   one's: nothing in a bootstrap comes from a message row.
//! - No wall clock: the instance meta is literal, and the frames keep the
//!   frozen dates of the hand-built fixture.

use std::sync::Arc;

use insta::assert_snapshot;
use mp_protocol::state::Bootstrap;
use mp_protocol::{Request, RequestId, JSONRPC_VERSION};
use serde_json::json;

use super::golden_frames::{
    calendar_fixture, calendar_view_cancelled_event_detail_app, command_palette_app,
    compose_wizard_with_body_app, contacts_fixture, drafts_fixture, drafts_view_attach_prompt_app,
    frame_snapshot, help_overlay_app, mail_fixture, mail_view_flagged_filter_app,
    mail_view_jump_date_prompt_app, mail_view_with_selection_app,
    mail_view_with_the_status_axis_app, mail_view_zoomed_list_app, mail_view_zoomed_preview_app,
    pin_theme, search_form_empty_app, search_form_filled_app, signatures_overlay_app, HEIGHT,
    WIDTH,
};
use crate::config::{AccountConfig, GlobalConfig, MailboxMapping, MailboxesConfig};
use crate::daemon::config::{ConfigState, ConfigStore};
use crate::daemon::dispatch::{ClientCtx, ClientKind};
use crate::daemon::runtime::InstanceMeta;
use crate::daemon::server::DaemonState;
use crate::daemon::state::Change;
use crate::tui::app::App;

/// The one account the fixture configures, and the one every frame shows.
///
/// One, not two: the sidebar titles itself after the account only when there is
/// more than one (`ui::sidebar`), and a second account would change the chrome
/// the hand-built frames were reviewed against without testing anything about a
/// bootstrap.
const ACCOUNT: &str = "fixture";

/// The per-mailbox totals the fixture commits before it bootstraps, keyed by
/// the slug [`crate::tui::app::build_mailboxes`] gives each role.
///
/// They are the four numbers the hand-built fixture assigns to
/// `App::mailbox_counts` (`vec![5, 2, 41, 128]`), so the daemon-built sidebar
/// has the same column to render. `unread` and `badge` stay 0: the sidebar's
/// count column shows the total alone, and a second number nothing renders
/// would be a fixture detail no frame could catch.
const TOTALS: [(&str, u64); 4] = [("inbox", 5), ("drafts", 2), ("sent", 41), ("archive", 128)];

// ---------------------------------------------------------------------------
// The daemon fixture
// ---------------------------------------------------------------------------

/// A bootstrap taken from a daemon, with everything that has to outlive it.
///
/// The data-root override is held here rather than dropped at the end of the
/// builder because `App::from_bootstrap` resolves per-account paths under the
/// data root, and a dropped tempdir would point the next resolution at the
/// developer's own tree.
struct Bootstrapped {
    bootstrap: Bootstrap,
    config: GlobalConfig,
    _data: crate::config::test_env::TestDataDir,
}

impl Bootstrapped {
    /// The daemon-backed variant of `hand`: every field the bootstrap owns is
    /// replaced by what `App::from_bootstrap` built out of it, and nothing else
    /// moves.
    ///
    /// Four fields, because a bootstrap carries exactly four things a frame can
    /// show: which accounts there are and how each one is doing, which
    /// mailboxes they have, what the counts are, and which account is active.
    /// The message rows, the cursor, the overlays and the preview memos stay
    /// the frozen fixture's - a bootstrap carries no message, so replacing them
    /// from one is P5-U3's query layer, not this unit's.
    fn variant(&self, mut hand: App) -> App {
        let built = App::from_bootstrap(self.config.clone(), &self.bootstrap);
        hand.accounts = built.accounts;
        hand.account_config = built.account_config;
        hand.mailboxes = built.mailboxes;
        hand.mailbox_counts = built.mailbox_counts;
        hand
    }

    /// The rendered frame of the daemon-backed variant of `hand`.
    fn frame(&self, hand: App) -> String {
        frame_snapshot(&mut self.variant(hand), WIDTH, HEIGHT)
    }
}

/// The configuration the fixture daemon is seeded from: one account, plus one
/// extra mailbox per entry in `extra`.
fn fixture_config(extra: &[&str]) -> GlobalConfig {
    GlobalConfig {
        accounts: vec![AccountConfig {
            name: ACCOUNT.to_string(),
            mailboxes: MailboxesConfig {
                extra: (!extra.is_empty()).then(|| {
                    extra
                        .iter()
                        .map(|server| MailboxMapping {
                            server: (*server).to_string(),
                        })
                        .collect()
                }),
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// The instance this daemon publishes: every field literal, so no frame can
/// carry a pid, a start time or a path that moves between runs.
fn meta(root: &std::path::Path) -> InstanceMeta {
    InstanceMeta {
        app_version: "0.0.0-golden".to_string(),
        protocol_min: mp_protocol::PROTOCOL_MIN,
        protocol_max: mp_protocol::PROTOCOL_MAX,
        instance_id: "golden-frames".to_string(),
        pid: 42,
        started_at: "2026-07-28T09:00:00Z".to_string(),
        data_dir: root.to_path_buf(),
        config_dir: root.to_path_buf(),
    }
}

/// The TUI's own connection context: a `tui` client at protocol 1 that agreed
/// on the one capability it uses here.
fn ctx() -> ClientCtx {
    ClientCtx {
        connection_id: 1,
        kind: ClientKind::Tui,
        protocol: 1,
        capabilities: vec!["state.bootstrap".to_string()],
    }
}

/// Run one future to completion on a runtime of this test's own.
///
/// A current-thread runtime rather than `#[tokio::test]`, because the golden
/// frames are synchronous tests and the daemon is reached exactly once per
/// fixture, before any rendering happens.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

/// Assemble a daemon over a fixture data root, commit `totals`, optionally
/// report the account ready, and return what `state.bootstrap` answers.
fn bootstrap_over_daemon(ready: bool, totals: &[(&str, u64)], extra: &[&str]) -> Bootstrapped {
    // Before the first `App` exists: `App::from_bootstrap` selects a theme the
    // way `App::new` does, and `theme::init` is a `OnceLock` whose first caller
    // wins for the whole binary.
    pin_theme();

    let data = crate::config::test_env::TestDataDir::new();
    let root = crate::config::mailypoppins_data_dir();
    let config = fixture_config(extra);
    let store = Arc::new(ConfigStore::new(
        root.join("config.toml"),
        ConfigState::Ok,
        config.clone(),
        // No account runtimes: this daemon starts none, so nothing it reports
        // depends on a store being openable, which is what keeps the frame a
        // function of the snapshot alone.
        false,
    ));
    let daemon = DaemonState::new(meta(&root), store);

    // The changes an account's runtime publishes when it has read its counts,
    // committed before the capture so they are part of the snapshot rather
    // than events arriving after it. Readiness last, because an account that
    // is ready with no counts yet is a different frame (`··` is about the
    // account, the numbers are about the mailboxes).
    for (slug, total) in totals {
        daemon.canonical.apply(Change::MailboxCounts {
            account: ACCOUNT.to_string(),
            mailbox: (*slug).to_string(),
            total: *total,
            unread: 0,
            badge: 0,
        });
    }
    if ready {
        daemon.canonical.apply(Change::AccountReady {
            account: ACCOUNT.to_string(),
        });
    }

    let request = Request {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: Some(RequestId::Num(1)),
        method: "state.bootstrap".to_string(),
        params: json!({}),
    };
    let outcome = block_on(daemon.dispatcher.dispatch(&ctx(), request))
        .expect("the daemon answers state.bootstrap");
    let bootstrap: Bootstrap =
        serde_json::from_value(outcome.result).expect("the bootstrap result decodes whole");

    Bootstrapped {
        bootstrap,
        config,
        _data: data,
    }
}

/// The fixture every frame below is built from: the four mailboxes with the
/// hand-built fixture's counts, the account ready.
fn ready_fixture() -> Bootstrapped {
    bootstrap_over_daemon(true, &TOTALS, &[])
}

// ---------------------------------------------------------------------------
// Daemon-backed golden frames
// ---------------------------------------------------------------------------

/// The oracle every frame below uses: build the fixture twice, once for the
/// daemon-backed variant and once as the hand-built reference, and assert the
/// two renders are identical.
///
/// `App` is not `Clone`, so the builder is passed rather than an app, and the
/// failure names the first line that differs rather than printing two 40-row
/// frames, because the thing a reader needs is which row moved.
fn same_frame(build: fn() -> App, label: &str) {
    let fixture = ready_fixture();
    let daemon = fixture.frame(build());
    let expected = frame_snapshot(&mut build(), WIDTH, HEIGHT);
    if daemon == expected {
        return;
    }
    match daemon
        .lines()
        .zip(expected.lines())
        .enumerate()
        .find(|(_, (left, right))| left != right)
    {
        Some((row, (left, right))) => panic!(
            "{label}: the daemon-built frame differs from the hand-built one at line {row}\n\
             daemon: {left}\n  hand: {right}"
        ),
        None => panic!(
            "{label}: the daemon-built frame is {} lines against the hand-built {}",
            daemon.lines().count(),
            expected.lines().count()
        ),
    }
}

/// The default mail view, built from a bootstrap.
#[test]
fn golden_mail_view_daemon() {
    same_frame(mail_fixture, "mail view");
}

/// The mail view with two rows toggle-selected.
#[test]
fn golden_mail_view_with_selection_daemon() {
    same_frame(mail_view_with_selection_app, "mail view with selection");
}

/// The mail view with the second status axis on screen (#TKT-0051).
#[test]
fn golden_mail_view_with_the_status_axis_daemon() {
    same_frame(mail_view_with_the_status_axis_app, "mail view status axis");
}

/// The flagged-only view (#0079), whose status line carries a `shown/total`
/// pair the sidebar counts have to agree with.
#[test]
fn golden_mail_view_flagged_filter_daemon() {
    same_frame(mail_view_flagged_filter_app, "mail view flagged filter");
}

/// The jump-to-date prompt armed (#0017).
#[test]
fn golden_mail_view_jump_date_prompt_daemon() {
    same_frame(mail_view_jump_date_prompt_app, "mail view jump-date prompt");
}

/// The preview pane zoomed (#TKT-0044): the sidebar is gone, so this is the
/// frame that proves a bootstrap-built app zooms like a hand-built one.
#[test]
fn golden_mail_view_zoomed_preview_daemon() {
    same_frame(mail_view_zoomed_preview_app, "mail view zoomed preview");
}

/// The email list zoomed (#TKT-0044).
#[test]
fn golden_mail_view_zoomed_list_daemon() {
    same_frame(mail_view_zoomed_list_app, "mail view zoomed list");
}

/// The calendar agenda plus the shared event card.
#[test]
fn golden_calendar_view_daemon() {
    same_frame(calendar_fixture, "calendar view");
}

/// The Calendar view with the cursor on the cancelled event (#0031).
#[test]
fn golden_calendar_view_cancelled_event_detail_daemon() {
    same_frame(
        calendar_view_cancelled_event_detail_app,
        "calendar cancelled event",
    );
}

/// The Contacts view (#TKT-0048).
#[test]
fn golden_contacts_view_daemon() {
    same_frame(contacts_fixture, "contacts view");
}

/// The Drafts view with a parse-skipped draft as an error row (#0080). The
/// Drafts mailbox is active here, so the frame also pins that the second
/// sidebar row a bootstrap reported is the one `active_mailbox` addresses.
#[test]
fn golden_drafts_view_with_a_parse_skip_daemon() {
    same_frame(drafts_fixture, "drafts view with a parse skip");
}

/// The attach-file prompt armed on a Drafts row (#0098).
#[test]
fn golden_drafts_view_attach_prompt_daemon() {
    same_frame(drafts_view_attach_prompt_app, "drafts attach prompt");
}

/// The help overlay over the dimmed mail view.
#[test]
fn golden_help_overlay_daemon() {
    same_frame(help_overlay_app, "help overlay");
}

/// The command palette (#0100) over the dimmed mail view.
#[test]
fn golden_command_palette_daemon() {
    same_frame(command_palette_app, "command palette");
}

/// The signatures overlay (#0107) in browse mode.
#[test]
fn golden_signatures_overlay_daemon() {
    same_frame(signatures_overlay_app, "signatures overlay");
}

/// The server-search form (#0086b), empty.
#[test]
fn golden_search_form_empty_daemon() {
    same_frame(search_form_empty_app, "search form empty");
}

/// The same form filled.
#[test]
fn golden_search_form_filled_daemon() {
    same_frame(search_form_filled_app, "search form filled");
}

/// A New compose wizard with the inline body field focused (#0097).
#[test]
fn golden_compose_wizard_with_body_daemon() {
    same_frame(compose_wizard_with_body_app, "compose wizard with body");
}

// ---------------------------------------------------------------------------
// The two frames no hand-built fixture has
// ---------------------------------------------------------------------------

/// The shell painted before any account is ready: the snapshot's account is
/// `opening`, so the sidebar shows the `··` loading marker instead of a count
/// and every count is zero, exactly as #0003 established for an account whose
/// store has not been opened yet.
///
/// This is the frame a daemon-backed TUI paints first, every time, and the one
/// the plan means by "the shell still paints before any store opens": the list
/// is whatever the query layer has (nothing, at P5-U2), the chrome is already
/// there.
#[test]
fn golden_opening_account_daemon() {
    // Counts committed *and* the account left `opening`: the marker is about
    // the account, so it must win over numbers the snapshot already carries.
    let fixture = bootstrap_over_daemon(false, &TOTALS, &[]);
    let frame = fixture.frame(mail_fixture());

    assert!(
        frame.contains("Inbox ··") && frame.contains("Drafts ··"),
        "an opening account shows the #0003 loading marker, not a count:\n{frame}"
    );
    assert!(
        !frame.contains("Inbox  5"),
        "an opening account shows no count at all:\n{frame}"
    );
    assert_snapshot!(frame);
}

/// A bootstrap that carries one mailbox row more renders a different frame.
///
/// The proof that the sidebar is the snapshot's and not a constant: a
/// `from_bootstrap` that hand-assigned the four mailboxes and ignored its
/// argument would pass every frame above and fail here. One row *more* rather
/// than one fewer because `build_mailboxes` emits the four roles
/// unconditionally, so a configured account cannot have fewer than four; the
/// count row below is the other half of the same proof.
#[test]
fn golden_extra_mailbox_daemon() {
    let extra = bootstrap_over_daemon(true, &TOTALS, &["Projekte"]);
    let frame = extra.frame(mail_fixture());

    assert!(
        frame.contains("Projekte"),
        "the fifth mailbox the bootstrap carried is missing from the sidebar:\n{frame}"
    );
    assert_ne!(
        frame,
        ready_fixture().frame(mail_fixture()),
        "one mailbox row more must change the frame"
    );
    assert_snapshot!(frame);
}

/// A bootstrap that carries a different count renders a different frame.
///
/// The count column is the narrowest thing a wrong snapshot can move, so it is
/// the sharpest test that the numbers travel: six in the inbox where the fixture
/// has five.
#[test]
fn a_different_count_in_the_bootstrap_changes_the_frame() {
    let moved = bootstrap_over_daemon(
        true,
        &[("inbox", 6), ("drafts", 2), ("sent", 41), ("archive", 128)],
        &[],
    );
    let frame = moved.frame(mail_fixture());

    assert!(
        frame.contains("Inbox  6"),
        "the sidebar shows the snapshot's total:\n{frame}"
    );
    assert_ne!(
        frame,
        ready_fixture().frame(mail_fixture()),
        "a different total must change the frame"
    );
}

/// Two bootstraps of the same fixture render the same frame, for every base
/// fixture: the daemon half of `frames_are_reproducible`.
///
/// Worth its own row beside that one because the daemon adds inputs a renderer
/// cannot: an instance id, a revision, a capability list and a tempdir path.
/// None of them may reach a frame.
#[test]
fn daemon_frames_are_reproducible() {
    for build in [
        mail_fixture as fn() -> App,
        calendar_fixture as fn() -> App,
        contacts_fixture as fn() -> App,
        drafts_fixture as fn() -> App,
    ] {
        let first = ready_fixture().frame(build());
        let second = ready_fixture().frame(build());
        assert_eq!(
            first, second,
            "a daemon-built frame is not reproducible across two bootstraps"
        );
    }
}
