//! The TUI's blocked senders (`B`, `sb`, PERSO-261) on the daemon's real
//! `sender.*` methods, over an in-process daemon ([`TestDaemon`]) whose list
//! file is checked on disk.
//!
//! The accounts have no server, so a block moves nothing and skips nothing
//! (a local-only account is in neither list); the counts and the skipped
//! sentence are `clients/tui/src/block_tests.rs`'s, against a door that
//! answers them.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};

use super::daemon::TestDaemon;
use crate::tui::app::{App, EmailEntry, MessageRef, Overlay};

fn received(from: &str) -> EmailEntry {
    EmailEntry {
        draft_sync: None,
        msg: Some(MessageRef::new(1)),
        draft_id: None,
        skip: None,
        selector: None,
        from: from.to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: "Offer".to_string(),
        status: "inbox".to_string(),
        date_display: "2026-11-06".to_string(),
        date_sort: "2026-11-06T10:00:00".to_string(),
        has_attachments: false,
        read: false,
        answered: false,
        forwarded: false,
        flagged: false,
        is_invite: false,
        conv: None,
    }
}

fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::from(KeyCode::Char(c)));
}

fn on_disk() -> Vec<String> {
    crate::daemon::senders::load(&crate::daemon::senders::list_path()).expect("the list file")
}

/// `B` then Enter writes the sender's address to the daemon's list, `j` the
/// domain; `sb` lists both from `sender.list`, and `u` removes the cursor
/// one through `sender.unblock`.
#[test]
fn b_blocks_through_the_daemon_and_sb_unblocks() {
    let _data = crate::config::test_env::TestDataDir::new();
    let mut app = App::default_for_tests();
    app.emails = Arc::new(vec![received("Offers <Offers@Shop.Example>")]);
    app.email_cache = vec![Some(Arc::clone(&app.emails))];
    app.mailbox_counts = vec![1];
    app.rebuild_visible();
    app.session = Some(TestDaemon::new(&["work", "home"]).session());

    press(&mut app, 'B');
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(on_disk(), ["offers@shop.example"]);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Blocked offers@shop.example; no Inbox mail from it")
    );
    press(&mut app, 'B');
    press(&mut app, 'j');
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(on_disk(), ["@shop.example", "offers@shop.example"]);

    press(&mut app, 's');
    press(&mut app, 'b');
    let Overlay::BlockedSenders(listed) = &app.overlay else {
        panic!("sb opens the blocked senders");
    };
    assert_eq!(listed.entries, ["@shop.example", "offers@shop.example"]);
    press(&mut app, 'u');
    assert_eq!(on_disk(), ["offers@shop.example"]);
    let Overlay::BlockedSenders(listed) = &app.overlay else {
        panic!("the list stays open after an unblock");
    };
    assert_eq!(listed.entries, ["offers@shop.example"]);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Unblocked @shop.example; its mail already in Junk stays there")
    );
}
