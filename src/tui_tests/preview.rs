//! The invitation card over a store the ingest path seeded (#0038 scope item
//! 6, #0126 P5-U10e).
//!
//! One row, and it is here rather than beside the card renderer because what
//! it asserts is a fold over the account's REPLY rows: the card shows the
//! *derived* answer, and deriving it needs `crate::reconcile`'s store half and
//! the fixture that ingests a REQUEST and its reply. The renderer it calls is
//! the TUI's, reached the way any other caller reaches it.

use crate::reconcile;
use crate::reconcile::tests::{fixture, reply_ics};
use ratatui::text::Line;

use crate::tui::ui::preview::event_card_lines;

/// The text of one rendered line, spans concatenated, which is how every
/// assertion about a card reads it.
fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The card renders the *derived* answer, not a stored one: with the sent
/// REPLY in the store, the same fold the agenda runs turns the invite's
/// `NEEDS-ACTION` into `Declined` on the card (#0038 scope item 6).
#[test]
fn the_card_shows_the_rsvp_derived_from_the_sent_reply() {
    let fx = fixture();
    let me = "me@example.com";
    fx.ingest_invite(
        "inbox",
        1,
        "Plan",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
         UID:uid-card\r\nSEQUENCE:0\r\nSUMMARY:Plan\r\nDTSTART:20260801T090000Z\r\n\
         ORGANIZER:mailto:org@example.com\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    fx.ingest_invite(
        "sent",
        2,
        "Declined: Plan",
        &reply_ics("uid-card", 0, me, "DECLINED", "20260710T120000Z"),
    );

    // The same derivation `App::load_message_invite` performs.
    let invites = reconcile::load_invites(&fx.store, &fx.blobs, "alice");
    let replies = reconcile::fold_replies(&invites);
    let request = invites
        .iter()
        .find(|i| i.method() == "REQUEST")
        .expect("the REQUEST is in the store");
    let mut event = crate::calendar::event_frontmatter(&request.parsed);
    let by_addr = replies.get("uid-card");
    reconcile::apply_replies(&mut event, request.parsed.sequence, by_addr);
    event.rsvp = reconcile::own_rsvp(&event, me, by_addr);

    let text: String = event_card_lines(&event, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Your RSVP: Declined"), "text=\n{text}");
    assert!(text.contains("me@example.com \u{2014} Declined"), "text=\n{text}");
}
