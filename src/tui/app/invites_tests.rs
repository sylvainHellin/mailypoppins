//! The agenda and the invitation card, through the daemon (P5-U10, #0124).
//!
//! `calendar.events`, `message.invite` and `message.ics` are the three methods
//! the `TUI_APP_STORE_RESIDUE` sites of [`super`] were waiting for: until this
//! unit `load_calendar_events`, `load_message_invite` and `load_message_ics`
//! opened the account's store themselves, because no registered method answered
//! an agenda row, a folded card or a raw iMIP blob.
//!
//! Every row here is an **equality oracle** in the shape
//! [`super::queries_tests`] established: the daemon-backed answer is compared
//! against the store-backed one, over one seeded store, in one process. That is
//! what makes the three methods a *move* rather than a second implementation
//! whose drift nobody would notice until a pty six months later.
//!
//! The fixture and both oracles come from [`crate::reconcile::tests`] rather
//! than being built here, and that is not tidiness: a `use crate::store::…` in
//! this file would widen the allow-list
//! `tests/architecture_boundaries.rs` drives to zero, which is the very thing
//! this unit exists to shrink.

use serde_json::json;

use crate::config::AccountConfig;
use crate::reconcile::tests::{invite_ics, reply_ics, AmbientFixture};
use crate::tui::app::App;
use crate::tui::queries::{calendar_events, message_ics, message_invite, Queries};
use crate::tui::test_daemon::TestDaemon;

use super::MessageRef;

/// The one account every fixture configures, and the one every call names.
const ACCOUNT: &str = "alice";

/// The account's own address, as both paths compute it: an
/// [`AccountConfig::default`] carries no `default_from`, so both read `""`.
fn self_address() -> String {
    crate::parse::extract_email_address(&AccountConfig::default().default_from)
}

/// A daemon over the fixture's data root, as a [`Queries`] door.
fn daemon() -> TestDaemon {
    TestDaemon::new(&[ACCOUNT])
}

/// An `App` on [`ACCOUNT`] with no session, i.e. the store-backed path.
fn app_without_session() -> App {
    let mut app = App::default_for_tests();
    app.account_config = AccountConfig {
        name: ACCOUNT.to_string(),
        ..Default::default()
    };
    app
}

/// One REQUEST with one attendee, and the REPLY that answers it.
fn seed_invited_and_answered(fx: &AmbientFixture) -> i64 {
    let row = fx.ingest(
        "inbox",
        1,
        "Invitation: Plan",
        Some(&invite_ics("uid-1", 0, &["bob@example.com"])),
    );
    fx.ingest(
        "inbox",
        2,
        "Accepted: Plan",
        Some(&reply_ics(
            "uid-1",
            0,
            "bob@example.com",
            "ACCEPTED",
            "20260720T100000Z",
        )),
    );
    row
}

// ---------------------------------------------------------------------------
// calendar.events
// ---------------------------------------------------------------------------

/// The agenda the daemon builds is the agenda the store-backed loader builds,
/// row for row and column for column.
#[test]
fn a_daemon_backed_agenda_matches_the_store_backed_one() {
    let fx = AmbientFixture::new(ACCOUNT);
    seed_invited_and_answered(&fx);
    fx.ingest("inbox", 3, "Just mail", None);

    let expected = fx.agenda(&self_address());
    let daemon = daemon();
    let actual = calendar_events(&daemon as &dyn Queries, ACCOUNT).expect("the agenda");

    assert_eq!(actual.len(), 1, "one REQUEST is one agenda row: {actual:?}");
    assert_eq!(
        format!("{actual:?}"),
        format!("{expected:?}"),
        "the daemon-backed agenda differs from the store-backed one"
    );
    assert_eq!(
        actual[0].event.attendees[0].status, "accepted",
        "the REPLY beside the REQUEST is folded into the row the daemon answers"
    );
}

/// An account with no invitations answers an empty agenda rather than
/// refusing: an empty calendar is the ordinary case.
#[test]
fn an_account_with_no_invites_has_an_empty_agenda() {
    let fx = AmbientFixture::new(ACCOUNT);
    fx.ingest("inbox", 1, "Just mail", None);
    let daemon = daemon();
    assert!(calendar_events(&daemon as &dyn Queries, ACCOUNT)
        .expect("the agenda")
        .is_empty());
}

/// The method takes `account` and nothing else, so a typo is `-32602` rather
/// than a silently ignored narrowing.
#[test]
fn the_agenda_method_refuses_a_parameter_it_does_not_know() {
    let _fx = AmbientFixture::new(ACCOUNT);
    let daemon = daemon();
    let refusal = daemon
        .call(
            "calendar.events",
            json!({"account": ACCOUNT, "mailbox": "inbox"}),
        )
        .expect_err("an unknown parameter is refused");
    assert!(
        format!("{refusal:#}").contains("mailbox"),
        "the refusal names the parameter it did not know: {refusal:#}"
    );
}

// ---------------------------------------------------------------------------
// message.invite
// ---------------------------------------------------------------------------

/// The card the daemon folds is the card the store-backed fold produces,
/// attendee statuses and all.
#[test]
fn a_daemon_backed_invitation_card_matches_the_store_backed_one() {
    let fx = AmbientFixture::new(ACCOUNT);
    let row = seed_invited_and_answered(&fx);

    let expected = fx
        .card(row, &self_address())
        .expect("the store-backed card");
    let daemon = daemon();
    let actual = message_invite(&daemon as &dyn Queries, ACCOUNT, MessageRef::new(row))
        .expect("the call")
        .expect("the daemon-backed card");

    assert_eq!(actual, expected);
    assert_eq!(actual.attendees[0].status, "accepted");
}

/// A row with no iMIP payload is `null`, not a refusal: a non-invite is the
/// ordinary case, and the preview shows no card for it exactly as before.
#[test]
fn a_message_without_an_invitation_answers_no_card() {
    let fx = AmbientFixture::new(ACCOUNT);
    let row = fx.ingest("inbox", 1, "Just mail", None);
    let daemon = daemon();
    assert!(
        message_invite(&daemon as &dyn Queries, ACCOUNT, MessageRef::new(row))
            .expect("the call")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// message.ics
// ---------------------------------------------------------------------------

/// The blob crosses the socket byte for byte, base64 and back.
#[test]
fn the_raw_ics_bytes_survive_the_wire() {
    let fx = AmbientFixture::new(ACCOUNT);
    let ics = invite_ics("uid-1", 0, &["bob@example.com"]);
    let row = fx.ingest("inbox", 1, "Invitation: Plan", Some(&ics));

    let expected = fx.ics(row).expect("the store-backed blob");
    let daemon = daemon();
    let actual = message_ics(&daemon as &dyn Queries, ACCOUNT, MessageRef::new(row))
        .expect("the call")
        .expect("the daemon-backed blob");

    assert_eq!(actual, expected);
    assert_eq!(String::from_utf8(actual).expect("utf-8"), ics);
}

/// A row with no payload answers `null`, which is what the RSVP key's "carries
/// no invitation to reply to" sentence is read off.
#[test]
fn a_message_without_an_invitation_answers_no_ics() {
    let fx = AmbientFixture::new(ACCOUNT);
    let row = fx.ingest("inbox", 1, "Just mail", None);
    let daemon = daemon();
    assert!(
        message_ics(&daemon as &dyn Queries, ACCOUNT, MessageRef::new(row))
            .expect("the call")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// The three App call sites
// ---------------------------------------------------------------------------

/// An `App` with a session reads all three through the daemon, and an `App`
/// without one reads them from the store: the same three answers either way,
/// which is what makes the sessionless path an oracle rather than a fallback
/// with a behaviour of its own.
#[test]
fn the_app_answers_the_same_three_things_with_and_without_a_session() {
    let fx = AmbientFixture::new(ACCOUNT);
    let row = seed_invited_and_answered(&fx);
    let msg = MessageRef::new(row);

    let store_app = app_without_session();
    let store_agenda = format!("{:?}", store_app.load_calendar_events());
    let store_card = store_app.load_message_invite(msg);
    let store_ics = store_app.load_message_ics(msg);

    let mut daemon_app = app_without_session();
    daemon_app.session = Some(daemon().session());
    assert_eq!(
        format!("{:?}", daemon_app.load_calendar_events()),
        store_agenda
    );
    assert_eq!(daemon_app.load_message_invite(msg), store_card);
    assert_eq!(daemon_app.load_message_ics(msg), store_ics);
    assert!(store_ics.is_some(), "the fixture really carries a payload");
}
