//! The blocked senders in the TUI (PERSO-261): `B` on a message, its chooser
//! between the sender's address and its `@domain`, the `sb` list with `u`,
//! and `sender.blocklist_changed` keeping an open list live.
//!
//! The daemon is a door that keeps the list the way `src/daemon/senders.rs`
//! does (lowercase, sorted, once) and answers a block with the counts and the
//! skipped account the real one would; `src/tui_tests/senders.rs` runs the
//! same keys against the real method bodies.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use mp_protocol::events::KIND_SENDER_BLOCKLIST_CHANGED;
use mp_protocol::sender::{BlockMoved, BlockSkipped, SenderBlocked};
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::app::{
    Action, App, BgResult, BlockChooser, EmailEntry, KeyAction, MailboxInfo, MailboxKind,
    MessageRef, Overlay, StatusLevel,
};

const INSTANCE: &str = "block-tests";
const SKIPPED: &str = "home has no Junk folder, so its Inbox keeps this sender's mail";

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// The daemon's list and every call made to it.
#[derive(Clone, Default)]
struct Daemon {
    entries: Arc<Mutex<Vec<String>>>,
    calls: Calls,
}

impl crate::queries::Queries for Daemon {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        let address = params["address"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        let mut entries = self.entries.lock().unwrap();
        match method {
            "sender.list" => Ok(json!({ "entries": *entries })),
            "sender.block" if address.ends_with("refused.example") => {
                Err(mp_client::session::Refused {
                    method: method.to_string(),
                    error: mp_protocol::RpcError {
                        code: -32602,
                        message: format!("'{address}' cannot be blocked"),
                        data: None,
                    },
                }
                .into())
            }
            "sender.block" => {
                let added = !entries.contains(&address);
                if added {
                    entries.push(address.clone());
                    entries.sort();
                }
                Ok(serde_json::to_value(SenderBlocked {
                    entry: address,
                    added,
                    entries: entries.clone(),
                    moved: vec![BlockMoved {
                        account: "work".to_string(),
                        mailbox: "Junk".to_string(),
                        count: 2,
                    }],
                    skipped: vec![BlockSkipped {
                        account: "home".to_string(),
                        reason: SKIPPED.to_string(),
                    }],
                })?)
            }
            "sender.unblock" => {
                let removed = entries.contains(&address);
                entries.retain(|e| *e != address);
                Ok(json!({ "entry": address, "removed": removed, "entries": *entries }))
            }
            other => Err(anyhow::anyhow!("unexpected {other}")),
        }
    }
}

fn received(row: i64, from: &str) -> EmailEntry {
    EmailEntry {
        draft_sync: None,
        msg: Some(MessageRef::new(row)),
        draft_id: None,
        skip: None,
        selector: None,
        from: from.to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: format!("Message {row}"),
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

fn draft() -> EmailEntry {
    EmailEntry {
        msg: None,
        draft_id: Some("a-draft".to_string()),
        status: "draft".to_string(),
        ..received(0, "me@example.com")
    }
}

/// An app listing `emails`, with a session on a daemon holding `entries`.
fn app(emails: Vec<EmailEntry>, entries: &[&str]) -> (App, Daemon) {
    let daemon = Daemon::default();
    *daemon.entries.lock().unwrap() = entries.iter().map(|e| e.to_string()).collect();
    let served = daemon.clone();
    let mut app = App::default_for_tests();
    app.emails = Arc::new(emails);
    app.email_cache = vec![Some(Arc::clone(&app.emails))];
    app.mailbox_counts = vec![app.emails.len()];
    app.rebuild_visible();
    app.session = Some(crate::session::Session::serving(move || served));
    (app, daemon)
}

fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::from(KeyCode::Char(c)));
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

fn calls(daemon: &Daemon) -> Vec<(String, Value)> {
    daemon.calls.lock().unwrap().clone()
}

fn chooser(app: &App) -> &BlockChooser {
    match &app.overlay {
        Overlay::BlockSender(chooser) => chooser,
        _ => panic!("the block chooser is not open"),
    }
}

fn listed(app: &App) -> Vec<String> {
    match &app.overlay {
        Overlay::BlockedSenders(overlay) => overlay.entries.clone(),
        _ => panic!("the blocked senders are not open"),
    }
}

/// The chooser reads the address a reply goes to, lowercase, and its domain;
/// a `From` with no address has nothing to block.
#[test]
fn the_chooser_offers_the_reply_address_and_its_domain() {
    let c = BlockChooser::for_sender("Offers <Offers@Shop.Example>").expect("an address");
    assert_eq!(c.entries(), ["offers@shop.example", "@shop.example"]);
    assert_eq!(c.entry(), "offers@shop.example");
    let first = BlockChooser::for_sender("x@y.org, Alice <alice@z.org>").expect("an address");
    assert_eq!(first.address, "alice@z.org");
    // A quoted local part that needs no quotes is offered unquoted, the
    // entry the daemon takes and matches it by.
    let quoted = BlockChooser::for_sender("John <\"John.Doe\"@Example.com>").expect("an address");
    assert_eq!(quoted.entries(), ["john.doe@example.com", "@example.com"]);
    for none in ["(unknown)", "", "Undisclosed recipients:;", "@example.com"] {
        assert_eq!(BlockChooser::for_sender(none), None, "{none:?}");
    }
}

/// `B` opens the chooser on the address; `j` moves to the domain and `k`
/// back, `d` and `a` pick each, and nothing is called until Enter.
#[test]
fn b_opens_the_chooser_on_the_sender_and_calls_nothing_until_enter() {
    let (mut app, daemon) = app(vec![received(1, "Offers <offers@shop.example>")], &[]);
    press(&mut app, 'B');
    assert_eq!(chooser(&app).entry(), "offers@shop.example");
    press(&mut app, 'j');
    assert_eq!(chooser(&app).entry(), "@shop.example");
    press(&mut app, 'k');
    assert_eq!(chooser(&app).entry(), "offers@shop.example");
    press(&mut app, 'd');
    assert_eq!(chooser(&app).entry(), "@shop.example");
    press(&mut app, 'a');
    assert_eq!(chooser(&app).entry(), "offers@shop.example");
    assert!(calls(&daemon).is_empty());
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.overlay, Overlay::None));
    assert!(calls(&daemon).is_empty(), "Esc blocks nothing");
}

/// Enter blocks the picked entry with `sender.block`, which moves the mail
/// already there, and the status line says what moved where and names the
/// account with no Junk folder in the daemon's words.
#[test]
fn enter_blocks_the_picked_entry_and_the_status_line_counts_the_moves() {
    let (mut app, daemon) = app(vec![received(1, "Offers <offers@shop.example>")], &[]);
    press(&mut app, 'B');
    press(&mut app, 'j');
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(
        calls(&daemon),
        [(
            "sender.block".to_string(),
            json!({"address": "@shop.example"})
        )]
    );
    assert_eq!(*daemon.entries.lock().unwrap(), ["@shop.example"]);
    let status = app.status_message.clone().expect("a status");
    assert_eq!(
        status,
        format!("Blocked @shop.example; 2 messages moved to Junk (work 2); {SKIPPED}")
    );
    assert!(
        matches!(
            app.status_log.back().map(|s| &s.level),
            Some(StatusLevel::Warning)
        ),
        "an account left alone is a warning"
    );
}

/// A block that moved the active account's Inbox mail reads its listings
/// again, since the daemon publishes the counts of the move and no row: the
/// Inbox and Junk caches go, the sidebar is recounted, and the open Inbox is
/// loaded afresh, so the moved messages, the cursor one included, leave the
/// list.
#[test]
fn a_block_that_moved_the_open_inbox_reloads_it() {
    let mailbox = |label: &str, id: &str, kind: MailboxKind| MailboxInfo {
        label: label.to_string(),
        icon: "",
        id: id.to_string(),
        kind,
        server_name: None,
    };
    let kept = received(2, "Alice <alice@example.org>");
    let (mut app, daemon) = app(
        vec![
            received(1, "Offers <offers@shop.example>"),
            kept.clone(),
            received(3, "Deals <deals@shop.example>"),
        ],
        &[],
    );
    app.account_config.name = "work".to_string();
    app.mailboxes = vec![
        mailbox("Inbox", "inbox", MailboxKind::Inbox),
        mailbox("Junk", "Junk", MailboxKind::Extra),
        mailbox("Sent", "sent", MailboxKind::Sent),
    ];
    let cached = Some(Arc::new(Vec::new()));
    app.email_cache = vec![Some(Arc::clone(&app.emails)), cached.clone(), cached];
    app.mailbox_counts = vec![3, 0, 0];

    press(&mut app, 'B');
    press(&mut app, 'j');
    key(&mut app, KeyCode::Enter);

    assert!(app.email_cache[0].is_none(), "the Inbox listing is stale");
    assert!(app.email_cache[1].is_none(), "the Junk listing is stale");
    assert!(app.email_cache[2].is_some(), "Sent did not move");
    assert!(
        calls(&daemon).iter().any(|(m, _)| m == "mailbox.list"),
        "the sidebar is recounted"
    );
    let load = app
        .pending_actions
        .iter()
        .find_map(|a| match a {
            Action::LoadMailbox {
                mailbox_idx,
                generation,
            } => Some((*mailbox_idx, *generation)),
            _ => None,
        })
        .expect("the open Inbox is read again");
    assert_eq!(load, (0, app.mailbox_load_generation));

    // The Inbox as the daemon lists it after the move.
    app.bg_count += 1;
    let account_index = app.active_account;
    crate::bg::handle_bg_result(
        &mut app,
        BgResult::MailboxLoaded {
            account_index,
            mailbox_idx: load.0,
            generation: load.1,
            entries: vec![kept],
        },
    );
    let listed: Vec<&str> = app.visible_emails().map(|e| e.from.as_str()).collect();
    assert_eq!(listed, ["Alice <alice@example.org>"]);
    assert_eq!(
        app.selected_email().map(|e| e.from.as_str()),
        Some("Alice <alice@example.org>"),
        "the cursor leaves the moved message"
    );
}

/// A block that moved another account's mail drops that account's cached
/// listings and leaves the open list alone.
#[test]
fn a_block_that_moved_another_account_drops_its_caches() {
    let (mut app, _daemon) = app(vec![received(1, "Offers <offers@shop.example>")], &[]);
    app.account_config.name = "home".to_string();
    let mut work = crate::app::AccountState::for_tests("work");
    work.email_cache = vec![Some(Arc::new(Vec::new()))];
    app.accounts = vec![crate::app::AccountState::for_tests("home"), work];
    press(&mut app, 'B');
    key(&mut app, KeyCode::Enter);
    assert!(app.accounts[1].email_cache[0].is_none());
    assert!(
        app.email_cache[0].is_some(),
        "the open list is not reloaded"
    );
    assert!(!app
        .pending_actions
        .iter()
        .any(|a| matches!(a, Action::LoadMailbox { .. })));
}

/// A refusal reaches the status line as the daemon's sentence, as an error.
#[test]
fn a_refused_block_is_the_daemons_sentence() {
    let (mut app, _daemon) = app(vec![received(1, "x@refused.example")], &[]);
    press(&mut app, 'B');
    press(&mut app, 'j');
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Cannot block @refused.example: '@refused.example' cannot be blocked")
    );
    assert!(matches!(
        app.status_log.back().map(|s| &s.level),
        Some(StatusLevel::Error)
    ));
}

/// A draft and a sender with no address are not offered.
#[test]
fn b_on_a_draft_or_an_addressless_sender_says_why() {
    for (email, why) in [
        (draft(), "Block needs a received message"),
        (
            received(1, "(unknown)"),
            "No address to block in '(unknown)'",
        ),
    ] {
        let (mut app, daemon) = app(vec![email], &[]);
        press(&mut app, 'B');
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.status_message.as_deref(), Some(why));
        assert!(calls(&daemon).is_empty());
    }
}

/// `sb` lists the daemon's entries; `u` unblocks the one under the cursor
/// and the answer's list replaces the overlay's; Esc closes.
#[test]
fn sb_lists_the_blocked_senders_and_u_unblocks_the_cursor_entry() {
    let (mut app, daemon) = app(vec![], &["@spam.example", "offers@shop.example"]);
    press(&mut app, 's');
    press(&mut app, 'b');
    assert_eq!(listed(&app), ["@spam.example", "offers@shop.example"]);
    press(&mut app, 'j');
    press(&mut app, 'u');
    assert_eq!(listed(&app), ["@spam.example"]);
    assert_eq!(
        calls(&daemon).last(),
        Some(&(
            "sender.unblock".to_string(),
            json!({"address": "offers@shop.example"})
        ))
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Unblocked offers@shop.example; its mail already in Junk stays there")
    );
    press(&mut app, 'u');
    assert!(listed(&app).is_empty());
    press(&mut app, 'u');
    assert_eq!(
        app.status_message.as_deref(),
        Some("No blocked sender to unblock")
    );
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.overlay, Overlay::None));
}

/// Another client's block or unblock reaches an open list: the event's
/// payload is the whole list, taken as it is, with the cursor kept on its
/// entry; with the list closed the event changes nothing.
#[test]
fn a_blocklist_changed_event_replaces_the_open_list() {
    let (mut app, daemon) = app(vec![], &["b@x.example", "c@x.example"]);
    app.apply_bootstrap(&Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision: 10,
        ..Default::default()
    });
    let changed = |revision: u64, entries: &[&str]| EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision,
        kind: KIND_SENDER_BLOCKLIST_CHANGED.to_string(),
        payload: json!({ "entries": entries }),
    };
    app.apply_event(&changed(11, &["a@x.example"]));
    assert!(matches!(app.overlay, Overlay::None));
    press(&mut app, 's');
    press(&mut app, 'b');
    press(&mut app, 'j');
    app.apply_event(&changed(12, &["a@x.example", "b@x.example", "c@x.example"]));
    assert_eq!(listed(&app), ["a@x.example", "b@x.example", "c@x.example"]);
    match &app.overlay {
        Overlay::BlockedSenders(o) => assert_eq!(o.selected_entry(), Some("c@x.example")),
        _ => unreachable!(),
    }
    let methods: Vec<String> = calls(&daemon)
        .into_iter()
        .map(|(m, _)| m)
        .filter(|m| m.starts_with("sender."))
        .collect();
    assert_eq!(
        methods,
        ["sender.list"],
        "the event is applied, not read again"
    );
}

/// Both actions are in the command palette, under the keys the help shows,
/// and run from it as from their keys.
#[test]
fn the_palette_offers_both_and_runs_them() {
    let actions = crate::app::palette_actions();
    let label = |action: KeyAction| {
        actions
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, l)| *l)
            .unwrap_or_else(|| panic!("{action:?} is not in the palette"))
    };
    assert_eq!(
        label(KeyAction::BlockSender),
        "Block the sender (address or @domain), its mail to Junk"
    );
    assert_eq!(
        label(KeyAction::OpenBlockedSenders),
        "Blocked senders (unblock one)"
    );
    assert!(crate::app::palette_match_text(KeyAction::BlockSender).ends_with(" B"));
    assert!(crate::app::palette_match_text(KeyAction::OpenBlockedSenders).ends_with(" sb"));

    let (mut app, _daemon) = app(vec![received(1, "a@b.example")], &["@spam.example"]);
    for (query, open) in [("blocked senders", "list"), ("block the sender", "chooser")] {
        press(&mut app, ':');
        for c in query.chars() {
            press(&mut app, c);
        }
        key(&mut app, KeyCode::Enter);
        match (&app.overlay, open) {
            (Overlay::BlockedSenders(o), "list") => assert_eq!(o.entries, ["@spam.example"]),
            (Overlay::BlockSender(c), "chooser") => assert_eq!(c.entry(), "a@b.example"),
            _ => panic!("{query:?} did not open the {open}"),
        }
        key(&mut app, KeyCode::Esc);
    }
}
