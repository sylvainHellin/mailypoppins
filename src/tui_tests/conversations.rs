//! The TUI's conversation list (#0142, PERSO-138 rollout step 4).
//!
//! Every row here runs the real daemon methods over a store seeded through
//! ingest: `message.list_conversations` collected off a row sink
//! ([`TestDaemon`]), `message.thread` with `prefer_mailbox` and
//! `include_drafts` for an expansion, and the per-message mutations an action
//! on a conversation row issues. The conversation is the ticket's shape with
//! one more inbox message, so an action on the collapsed row has two members
//! to reach:
//!
//! ```text
//! inbox  Mon 1 Jan  Ivana   Frage zum Bericht         unread
//! sent   Tue 2 Jan  Alice   Re: Frage zum Bericht
//! inbox  Wed 3 Jan  Ivana   Re: Frage zum Bericht     unread
//! sent   Thu 4 Jan  Alice   Re: Frage zum Bericht
//! draft  Fri 5 Jan  (the draft reply to the inbox follow-up)
//! inbox  Tue 2 Jan  Bot     Wochenbericht             a conversation of one
//! ```
//!
//! Two golden frames (the conversation collapsed, and expanded with the cursor
//! on its draft member) are minted here, with two of the `tt` conversation
//! overlay opened over each (PERSO-148); the frames of the flat list are the
//! hand-built suite's and are untouched by this mode, which is off unless a
//! test turns it on.

use insta::assert_snapshot;
use serde_json::{json, Value};

use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use super::daemon::{CallLog, TestDaemon};
use crate::config::AccountConfig;
use crate::parse::FetchedEmail;
use crate::tui::app::{
    build_mailboxes, mailbox_key, Action, App, BgResult, EntryKey, Focus, MailboxKind, MessageRef,
};
use crate::tui::bg::handle_bg_result;
use crate::tui::commands::dispatch;
use crate::tui::queries::{list_mailbox, Queries};
use crate::tui::ui::golden_frames::{frame_snapshot, pin_theme, text_rows, HEIGHT, WIDTH};

const ACCOUNT: &str = "alice";

const INSTANCE: &str = "tui-test";

const WATERMARK: u64 = 10;

/// The conversation's root, its `thread_id`.
const QUESTION: &str = "<question@example.com>";
const FIRST_REPLY: &str = "<first-reply@example.com>";
const FOLLOW_UP: &str = "<follow-up@example.com>";
const SECOND_REPLY: &str = "<second-reply@example.com>";

/// The draft that answers the follow-up.
const DRAFT_ID: &str = "d0c5a1e2f3b4c5d6";

/// The row ids the rows address, by name.
struct Seeded {
    question: i64,
    first_reply: i64,
    follow_up: i64,
    second_reply: i64,
    lone: i64,
}

/// A data root with the conversation, the lone message and the draft, and an
/// `App` on its inbox in conversation mode over a recording session, loaded.
struct Fixture {
    app: App,
    log: CallLog,
    ids: Seeded,
    _data: crate::config::test_env::TestDataDir,
}

impl Fixture {
    fn new() -> Fixture {
        pin_theme();
        let data = crate::config::test_env::TestDataDir::new();
        std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
        let ids = seed();
        let log = CallLog::default();
        let mut app = App::default_for_tests();
        app.account_config = AccountConfig {
            name: ACCOUNT.to_string(),
            ..Default::default()
        };
        app.accounts = vec![account_state()];
        app.mailboxes = build_mailboxes(&app.account_config);
        app.mailbox_counts = vec![0; app.mailboxes.len()];
        app.email_cache = vec![None; app.mailboxes.len()];
        app.active_mailbox = app
            .find_mailbox_by_kind(MailboxKind::Inbox)
            .expect("an inbox");
        app.sidebar_index = app.active_mailbox;
        app.session = Some(TestDaemon::new(&[ACCOUNT]).recording_session(log.clone()));
        app.apply_bootstrap(&Bootstrap {
            instance_id: INSTANCE.to_string(),
            revision: WATERMARK,
            ..Bootstrap::default()
        });
        app.set_conversation_mode(true);
        assert_eq!(run_load(&mut app), Some(true), "the first listing lands");
        Fixture {
            app,
            log,
            ids,
            _data: data,
        }
    }

    /// The methods called since the last [`Self::forget`], in order.
    fn methods(&self) -> Vec<String> {
        self.log
            .lock()
            .expect("the call log")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    /// The `row_id`s `method` was called for, in order.
    fn rows_of(&self, method: &str) -> Vec<i64> {
        self.log
            .lock()
            .expect("the call log")
            .iter()
            .filter(|(m, _)| m == method)
            .filter_map(|(_, params)| params["row_id"].as_i64())
            .collect()
    }

    fn forget(&self) {
        self.log.lock().expect("the call log").clear();
    }

    /// Run `action` through the daemon-routed dispatcher, as the loop does.
    fn dispatch(&mut self, action: Action) {
        let door = self.app.session.as_ref().expect("a session").handle();
        assert!(dispatch(&mut self.app, &door, &action), "{action:?}");
    }

    fn key(&mut self, code: crossterm::event::KeyCode) {
        self.app.handle_key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::NONE,
        ));
    }

    fn char(&mut self, c: char) {
        self.key(crossterm::event::KeyCode::Char(c));
    }

    /// The list as `(contact cell, subject)` of each visible row: a header
    /// says `H`, a member its mailbox.
    fn shape(&self) -> Vec<String> {
        self.app
            .visible_emails()
            .map(|e| match (e.header(), e.member_mailbox()) {
                (Some(h), _) => format!("H{} {}", h.count, e.subject),
                (None, Some(mailbox)) => format!("  {mailbox}"),
                (None, None) => format!("F {}", e.subject),
            })
            .collect()
    }
}

/// Run the oldest queued mailbox load the way `Action::LoadMailbox` does,
/// answering whether it was applied, or `None` when none was queued.
fn run_load(app: &mut App) -> Option<bool> {
    let at = app
        .pending_actions
        .iter()
        .position(|a| matches!(a, Action::LoadMailbox { .. }))?;
    let Some(Action::LoadMailbox {
        mailbox_idx,
        generation,
    }) = app.pending_actions.remove(at)
    else {
        unreachable!("the position was a load");
    };
    let mailbox = mailbox_key(&app.mailboxes[mailbox_idx]);
    let expanded = app
        .mailbox_lists_conversations(mailbox_idx)
        .then(|| app.expanded_threads());
    let entries = {
        let door: &dyn Queries = app.session.as_ref().expect("a session");
        list_mailbox(door, ACCOUNT, &mailbox, expanded.as_deref()).expect("the mailbox lists")
    };
    let current = generation == app.mailbox_load_generation;
    app.bg_count += 1;
    handle_bg_result(
        app,
        BgResult::MailboxLoaded {
            account_index: app.active_account,
            mailbox_idx,
            generation,
            entries,
        },
    );
    Some(current)
}

/// How many mailbox loads are queued.
fn queued_loads(app: &App) -> usize {
    app.pending_actions
        .iter()
        .filter(|a| matches!(a, Action::LoadMailbox { .. }))
        .count()
}

/// Seed the store and the drafts directory, answering the row ids.
fn seed() -> Seeded {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store");
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    let ivana = "Ivana Petrova <ivana@example.com>";
    let alice = "Alice Martin <alice@example.com>";
    let ingest = |mailbox: &str, uid: i64, email: FetchedEmail, parent: Option<&str>| {
        let raw = parent.map(|p| format!("In-Reply-To: {p}\r\nReferences: {QUESTION} {p}\r\n\r\n"));
        crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: ACCOUNT,
                mailbox,
                uid,
                email: &email,
                raw: raw.as_deref().map(str::as_bytes),
            },
        )
        .expect("ingest a fixture message")
        .row_id
    };
    let question = ingest(
        "inbox",
        1,
        message(
            QUESTION,
            ivana,
            "Frage zum Bericht",
            "Mon, 1 Jan 2024 08:00:00 +0000",
            false,
        ),
        None,
    );
    let first_reply = ingest(
        "sent",
        1,
        message(
            FIRST_REPLY,
            alice,
            "Re: Frage zum Bericht",
            "Tue, 2 Jan 2024 09:00:00 +0000",
            true,
        ),
        Some(QUESTION),
    );
    let lone = ingest(
        "inbox",
        2,
        message(
            "<lone@example.com>",
            "Bot <bot@example.com>",
            "Wochenbericht",
            "Tue, 2 Jan 2024 11:00:00 +0000",
            true,
        ),
        None,
    );
    let follow_up = ingest(
        "inbox",
        3,
        message(
            FOLLOW_UP,
            ivana,
            "Re: Frage zum Bericht",
            "Wed, 3 Jan 2024 10:00:00 +0000",
            false,
        ),
        Some(FIRST_REPLY),
    );
    let second_reply = ingest(
        "sent",
        2,
        message(
            SECOND_REPLY,
            alice,
            "Re: Frage zum Bericht",
            "Thu, 4 Jan 2024 12:00:00 +0000",
            true,
        ),
        Some(FOLLOW_UP),
    );
    drop(store);

    let drafts = crate::config::drafts_dir(ACCOUNT);
    std::fs::create_dir_all(&drafts).expect("the drafts directory");
    std::fs::write(
        drafts.join("2024-01-05-re-frage.md"),
        format!(
            "---\nid: {DRAFT_ID}\nto: ivana@example.com\ncc:\nbcc:\nsubject: \"Re: Frage zum Bericht\"\n\
             status: draft\nfrom: alice@example.com\ndate: Fri, 5 Jan 2024 09:30:00 +0000\n\
             in_reply_to: \"{FOLLOW_UP}\"\nreply_to:\nattachments:\n---\n\n\
             Hallo Ivana,\n\nder Bericht folgt morgen.\n"
        ),
    )
    .expect("write the draft");
    crate::store::drafts::refresh_account(ACCOUNT).expect("index the draft");

    Seeded {
        question,
        first_reply,
        follow_up,
        second_reply,
        lone,
    }
}

fn message(id: &str, from: &str, subject: &str, date: &str, seen: bool) -> FetchedEmail {
    FetchedEmail {
        from: from.into(),
        to: "Alice Martin <alice@example.com>".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.into(),
        date: date.into(),
        body_text: format!("Text von {subject} ({id})\n"),
        html_body: None,
        has_attachments: false,
        message_id: Some(id.to_string()),
        attachments: Vec::new(),
        flags: crate::types::MessageFlags::seen(seen),
        calendar_ics: None,
        event: None,
    }
}

/// A minimal `AccountState`, as `src/tui_tests/events.rs` builds one.
fn account_state() -> crate::tui::app::AccountState {
    crate::tui::app::AccountState {
        account_config: AccountConfig {
            name: ACCOUNT.to_string(),
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
        has_unseen: false,
        sync_health: crate::sync_health::SyncHealth::default(),
    }
}

fn event(revision: u64, kind: &str, payload: Value) -> EventEnvelope {
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision,
        kind: kind.to_string(),
        payload,
    }
}

/// The list rows of a frame rendered with the list zoomed, so each line is one
/// list row and nothing of another pane.
fn zoomed_list_lines(app: &mut App) -> Vec<String> {
    app.focus = Focus::List;
    app.zoomed = true;
    let backend = ratatui::backend::TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
    app.terminal_width = WIDTH;
    app.terminal_height = HEIGHT;
    terminal
        .draw(|frame| crate::tui::ui::view(app, frame))
        .expect("a frame");
    let text = text_rows(terminal.backend().buffer());
    app.zoomed = false;
    text.lines()
        .map(|line| {
            line.split_once('|')
                .map_or(line, |(_, row)| row)
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The rows and the frames
// ---------------------------------------------------------------------------

/// The inbox lists the conversation as one row with its count, its draft
/// marker and the date of its newest message, bold while a member here is
/// unread; the lone message is a conversation of one.
#[test]
fn golden_conversation_collapsed() {
    let mut fx = Fixture::new();
    assert_eq!(
        fx.shape(),
        ["H4 Re: Frage zum Bericht", "H1 Wochenbericht"],
        "one row per conversation, newest first"
    );
    let header = fx.app.emails[0].header().expect("a header").clone();
    assert_eq!(header.thread_id, QUESTION);
    assert_eq!(header.unread, 2);
    assert!(header.has_draft);
    assert_eq!(
        header.member_ids,
        [
            MessageRef::new(fx.ids.follow_up),
            MessageRef::new(fx.ids.question)
        ],
        "the inbox members, newest first"
    );
    assert_eq!(header.date_display, "2024-01-04", "the later sent reply's");
    assert!(!fx.app.emails[0].read, "drawn bold");
    assert_eq!(
        fx.app.emails[0].msg,
        Some(MessageRef::new(fx.ids.follow_up)),
        "the row previews its newest inbox member"
    );

    let frame = frame_snapshot(&mut fx.app, WIDTH, HEIGHT);
    assert!(
        frame.contains("(4) \u{f03eb} Re: Frage"),
        "the count and the draft marker:\n{frame}"
    );
    assert_snapshot!(frame);

    // The list zoomed is the wide layout, whose contact column carries the
    // participants and the count.
    fx.app.zoomed = true;
    let wide = frame_snapshot(&mut fx.app, WIDTH, HEIGHT);
    assert!(
        wide.contains("Alice Mart\u{2026} (4)"),
        "the participants:\n{wide}"
    );
    assert!(
        wide.contains("Bot"),
        "a conversation of one shows its sender:\n{wide}"
    );
    assert_snapshot!("golden_conversation_collapsed_wide", wide);
}

/// `Enter` expands the collapsed conversation and keeps the cursor on it; its
/// members are the draft and the four messages, newest first like the
/// mailbox around them, each labelled with its mailbox, the question as its
/// inbox copy. The frame parks the cursor on the draft member, whose preview
/// is the draft's own text.
#[test]
fn golden_conversation_expanded() {
    let mut fx = Fixture::new();
    fx.forget();
    fx.key(crossterm::event::KeyCode::Enter);

    assert_eq!(fx.app.list_index, 0, "the cursor stays on the header");
    assert_eq!(
        fx.app.focus,
        Focus::List,
        "a collapsed header expands, it does not read"
    );
    assert_eq!(fx.methods(), ["message.thread"]);
    {
        let log = fx.log.lock().expect("the call log");
        let (_, params) = &log[0];
        assert_eq!(params["prefer_mailbox"], json!("inbox"));
        assert_eq!(params["include_drafts"], json!(true));
    }
    assert_eq!(
        fx.shape(),
        [
            "H4 Re: Frage zum Bericht",
            "  drafts",
            "  sent",
            "  inbox",
            "  sent",
            "  inbox",
            "H1 Wochenbericht",
        ]
    );
    let members: Vec<Option<EntryKey>> = fx.app.emails[1..6].iter().map(|e| e.key()).collect();
    assert_eq!(
        members,
        [
            Some(EntryKey::Draft(DRAFT_ID.to_string())),
            Some(EntryKey::Msg(MessageRef::new(fx.ids.second_reply))),
            Some(EntryKey::Msg(MessageRef::new(fx.ids.follow_up))),
            Some(EntryKey::Msg(MessageRef::new(fx.ids.first_reply))),
            Some(EntryKey::Msg(MessageRef::new(fx.ids.question))),
        ]
    );

    fx.char('j');
    assert_eq!(
        fx.app.selected_email().and_then(|e| e.draft_id.clone()),
        Some(DRAFT_ID.to_string())
    );
    let frame = frame_snapshot(&mut fx.app, WIDTH, HEIGHT);
    for label in ["\u{2514} Inbox", "\u{2514} Sent", "\u{2514} Drafts"] {
        assert!(frame.contains(label), "{label} is missing:\n{frame}");
    }
    assert!(
        frame.contains("der Bericht folgt morgen."),
        "the draft member previews the draft:\n{frame}"
    );
    assert_snapshot!(frame);

    fx.app.zoomed = true;
    let wide = frame_snapshot(&mut fx.app, WIDTH, HEIGHT);
    assert_snapshot!("golden_conversation_expanded_wide", wide);
}

/// The conversation overlay's rows as `(row id, mailbox, current)`, oldest
/// first, and its cursor.
fn thread_overlay(app: &App) -> (Vec<(i64, String, bool)>, usize) {
    let crate::tui::app::Overlay::Thread(overlay) = &app.overlay else {
        panic!("the conversation overlay is open");
    };
    let rows = overlay
        .messages
        .iter()
        .map(|m| (m.msg.row_id(), m.mailbox.clone(), m.current))
        .collect();
    (rows, overlay.selected)
}

/// `tt` on a collapsed conversation row opens the conversation overlay on the
/// message the row previews, its newest inbox member: the four messages
/// oldest first across both mailboxes, the draft not among them, the caret
/// and the cursor on the follow-up.
#[test]
fn golden_thread_overlay_collapsed() {
    let mut fx = Fixture::new();
    fx.char('t');
    fx.char('t');
    let (rows, selected) = thread_overlay(&fx.app);
    assert_eq!(
        rows,
        [
            (fx.ids.question, "inbox".to_string(), false),
            (fx.ids.first_reply, "sent".to_string(), false),
            (fx.ids.follow_up, "inbox".to_string(), true),
            (fx.ids.second_reply, "sent".to_string(), false),
        ]
    );
    assert_eq!(selected, 2, "the cursor starts on the current message");

    let frame = frame_snapshot(&mut fx.app, WIDTH, HEIGHT);
    assert!(
        frame.contains("Conversation (4): Re: Frage zum Bericht"),
        "the title:\n{frame}"
    );
    assert_snapshot!(frame);
}

/// `tt` on an expanded member opens the overlay on that member: from the
/// second reply in Sent, the caret and the cursor sit on the last row while
/// the conversation behind the overlay stays expanded.
#[test]
fn golden_thread_overlay_expanded() {
    let mut fx = Fixture::new();
    fx.char('l');
    // Newest first: the draft, then the second reply.
    fx.char('j');
    fx.char('j');
    assert_eq!(
        fx.app.selected_email_ref(),
        Some(MessageRef::new(fx.ids.second_reply))
    );
    fx.char('t');
    fx.char('t');
    let (rows, selected) = thread_overlay(&fx.app);
    let current: Vec<i64> = rows
        .iter()
        .filter(|(_, _, current)| *current)
        .map(|(id, _, _)| *id)
        .collect();
    assert_eq!(current, [fx.ids.second_reply]);
    assert_eq!(rows.len(), 4, "the draft is not a member of the overlay");
    assert_eq!(selected, 3);

    let frame = frame_snapshot(&mut fx.app, WIDTH, HEIGHT);
    assert!(
        frame.contains("Conversation (4): Re: Frage zum Bericht"),
        "the title:\n{frame}"
    );
    assert_snapshot!(frame);
}

/// A conversation of one message renders exactly as the flat row does.
#[test]
fn a_conversation_of_one_renders_as_the_flat_row() {
    let mut fx = Fixture::new();
    let grouped = zoomed_list_lines(&mut fx.app);
    fx.app.set_conversation_mode(false);
    assert_eq!(run_load(&mut fx.app), Some(true));
    assert!(
        fx.app.emails.iter().all(|e| e.conv.is_none()),
        "the flat list"
    );
    let flat = zoomed_list_lines(&mut fx.app);

    let row = |lines: &[String]| {
        lines
            .iter()
            .find(|line| line.contains("Wochenbericht"))
            .cloned()
            .expect("the lone row is drawn")
    };
    assert_eq!(row(&grouped), row(&flat));
}

/// `Enter` on an expanded header reads it; `h` from any member folds the
/// conversation and parks the cursor on its header; `l` opens it again.
#[test]
fn enter_reads_an_expanded_row_and_l_and_h_expand_and_fold() {
    let mut fx = Fixture::new();
    fx.char('l');
    assert_eq!(fx.app.emails.len(), 7, "five members under the header");
    assert_eq!(fx.app.list_index, 0);

    fx.char('j');
    fx.char('j');
    fx.char('h');
    assert_eq!(fx.shape(), ["H4 Re: Frage zum Bericht", "H1 Wochenbericht"]);
    assert_eq!(
        fx.app.list_index, 0,
        "the fold parks the cursor on the header"
    );

    fx.char('l');
    fx.key(crossterm::event::KeyCode::Enter);
    assert_eq!(fx.app.focus, Focus::Preview, "an expanded header reads");
    assert!(
        fx.app
            .pending_actions
            .iter()
            .any(|a| matches!(a, Action::MarkAsRead(m) if m.row_id() == fx.ids.follow_up)),
        "reading the header marks its latest read"
    );

    // A conversation of one reads on the first `Enter`.
    fx.app.focus = Focus::List;
    fx.app.list_index = fx.app.visible.len() - 1;
    assert_eq!(
        fx.app.selected_email_ref(),
        Some(MessageRef::new(fx.ids.lone))
    );
    fx.key(crossterm::event::KeyCode::Enter);
    assert_eq!(fx.app.focus, Focus::Preview);
}

/// Reading a member from another mailbox opens it by its row id where the
/// list is: the sidebar stays on the inbox and the preview is that message.
#[test]
fn a_member_from_another_mailbox_previews_without_switching_mailbox() {
    let mut fx = Fixture::new();
    let inbox = fx.app.active_mailbox;
    fx.char('l');
    // Newest first: the draft, the second reply, the follow-up, then the
    // first reply.
    for _ in 0..4 {
        fx.char('j');
    }
    assert_eq!(
        fx.app.selected_email().and_then(|e| e.member_mailbox()),
        Some("sent")
    );
    fx.key(crossterm::event::KeyCode::Enter);
    assert_eq!(fx.app.focus, Focus::Preview);
    assert_eq!(fx.app.active_mailbox, inbox, "no mailbox switch");
    fx.app.refresh_preview_body();
    assert!(
        fx.app.preview_body.text().contains(FIRST_REPLY),
        "the preview is the sent reply: {:?}",
        fx.app.preview_body.text()
    );
}

/// An action on the collapsed row reaches its members in the listed mailbox
/// and nothing in Sent: marking it read clears its unread and its bold, and
/// archiving it leaves the conversation's Sent members where they are.
#[test]
fn actions_on_a_collapsed_row_reach_its_members_here() {
    let mut fx = Fixture::new();
    fx.forget();
    fx.dispatch(Action::ToggleRead);
    let mut marked = fx.rows_of("message.set_read");
    marked.sort_unstable();
    let mut inbox = vec![fx.ids.question, fx.ids.follow_up];
    inbox.sort_unstable();
    assert_eq!(marked, inbox, "one set_read per inbox member");
    let header = fx.app.emails[0].header().expect("a header");
    assert_eq!(header.unread, 0);
    assert!(fx.app.emails[0].read, "no longer bold");

    fx.forget();
    fx.dispatch(Action::Archive);
    let mut archived = fx.rows_of("message.archive");
    archived.sort_unstable();
    assert_eq!(
        archived, inbox,
        "one archive per inbox member, nothing in Sent"
    );
    assert_eq!(
        fx.shape(),
        ["H1 Wochenbericht"],
        "the conversation left the inbox"
    );
    let answer = fx
        .app
        .session
        .as_ref()
        .expect("a session")
        .call(
            "message.list",
            json!({"account": ACCOUNT, "mailbox": "sent", "limit": null}),
        )
        .expect("the sent listing");
    assert_eq!(
        answer["messages"].as_array().map(Vec::len),
        Some(2),
        "the replies stayed in Sent"
    );
}

/// Every expanded member carries its canonical selector, which is what `y`
/// copies: a message's names its own mailbox and Message-ID, a draft's the
/// drafts namespace, and the header keeps the one the listing sent.
#[test]
fn an_expanded_member_has_a_selector_to_copy() {
    let mut fx = Fixture::new();
    let header = fx.app.emails[0].selector.clone();
    fx.char('l');
    let selectors: Vec<Option<String>> = fx.app.emails[0..6]
        .iter()
        .map(|e| e.selector.clone())
        .collect();
    let message = |mailbox: &str, id: &str| {
        Some(format!(
            "mp://{ACCOUNT}/{mailbox}/{}",
            id.trim_matches(['<', '>'])
        ))
    };
    assert_eq!(
        selectors,
        [
            header,
            Some(format!("mp://{ACCOUNT}/drafts/{DRAFT_ID}")),
            message("sent", SECOND_REPLY),
            message("inbox", FOLLOW_UP),
            message("sent", FIRST_REPLY),
            message("inbox", QUESTION),
        ]
    );
}

/// On an expanded member an action applies to that member alone, whatever
/// mailbox it lives in; a selection of conversation rows is the union of their
/// members here.
#[test]
fn an_action_on_a_member_reaches_that_member_alone() {
    let mut fx = Fixture::new();
    fx.char('l');
    // Newest first: the draft, the second reply, the follow-up, then the
    // first reply.
    for _ in 0..4 {
        fx.char('j');
    }
    fx.forget();
    fx.dispatch(Action::ToggleFlag);
    assert_eq!(fx.rows_of("message.set_flag"), [fx.ids.first_reply]);
    assert!(fx.app.selected_email().is_some_and(|e| e.flagged));

    fx.app.list_index = 0;
    fx.app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('a'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    let mut selected: Vec<i64> = fx
        .app
        .selection
        .iter()
        .filter_map(|k| k.msg())
        .map(MessageRef::row_id)
        .collect();
    selected.sort_unstable();
    let mut want = vec![fx.ids.question, fx.ids.follow_up, fx.ids.lone];
    want.sort_unstable();
    assert_eq!(selected, want, "the union of the rows' members here");
}

/// A conversation list's load sets the sidebar count to the messages of the
/// listed mailbox, not to its conversations or its expanded rows: three in
/// the inbox (the question, the follow-up and the lone message), with the
/// conversation collapsed or expanded, and one after the conversation's two
/// inbox members are archived (PERSO-146).
#[test]
fn the_sidebar_count_follows_a_conversation_list_load() {
    let mut fx = Fixture::new();
    let inbox = fx.app.active_mailbox;
    assert_eq!(fx.app.mailbox_counts[inbox], 3, "the first load");

    fx.char('l');
    assert_eq!(fx.app.emails.len(), 7);
    fx.app.mailbox_counts[inbox] = 0;
    fx.app.reload_current_mailbox();
    assert_eq!(run_load(&mut fx.app), Some(true));
    assert_eq!(
        fx.app.mailbox_counts[inbox], 3,
        "the expanded members are not counted"
    );

    fx.app.list_index = 0;
    fx.dispatch(Action::Archive);
    assert_eq!(
        fx.app.mailbox_counts[inbox], 1,
        "the archive takes the conversation's two inbox members"
    );
}

/// The move picker on an expanded member leaves out the mailbox the member
/// lives in rather than the listed one: from the first reply in Sent it
/// offers the inbox and not Sent, and from the header it offers Sent and not
/// the inbox (PERSO-146).
#[test]
fn the_move_picker_leaves_out_the_mailbox_a_member_lives_in() {
    let mut fx = Fixture::new();
    // The fixture's account names no server folders; a move needs them.
    for mailbox in fx.app.mailboxes.iter_mut() {
        if mailbox.id != "drafts" {
            mailbox.server_name = Some(mailbox.label.clone());
        }
    }
    let picked = |app: &App| -> Vec<String> {
        let crate::tui::app::Overlay::Mailbox(picker) = &app.overlay else {
            panic!("the move picker is open");
        };
        picker
            .candidates
            .iter()
            .map(|(i, _)| mailbox_key(&app.mailboxes[*i]))
            .collect()
    };
    fx.char('M');
    let from_header = picked(&fx.app);
    assert!(
        !from_header.contains(&"inbox".to_string()),
        "{from_header:?}"
    );
    assert!(from_header.contains(&"sent".to_string()), "{from_header:?}");
    fx.key(crossterm::event::KeyCode::Esc);

    fx.char('l');
    for _ in 0..4 {
        fx.char('j');
    }
    assert_eq!(
        fx.app.selected_email_ref(),
        Some(MessageRef::new(fx.ids.first_reply))
    );
    fx.char('M');
    let from_member = picked(&fx.app);
    assert!(
        from_member.contains(&"inbox".to_string()),
        "{from_member:?}"
    );
    assert!(
        !from_member.contains(&"sent".to_string()),
        "{from_member:?}"
    );
}

/// Any change of the account re-streams the list, in any mailbox: the
/// `counts` invalidate the daemon announces a message change with, a row
/// delta, the removal of a message or a draft, `draft.changed`. A burst costs
/// one stream in flight and one owed, and an expanded conversation is
/// expanded again by the re-stream.
#[test]
fn deltas_restream_the_list_coalesced() {
    let mut fx = Fixture::new();
    fx.char('l');
    fx.app.pending_actions.clear();
    let row = json!({"id": fx.ids.first_reply, "uid": 1, "subject": "Re: Frage zum Bericht"});

    let mut revision = WATERMARK;
    let mut next = || {
        revision += 1;
        revision
    };
    for mailbox in ["sent", "inbox", "archive"] {
        fx.app.apply_event(&event(
            next(),
            "state.invalidate",
            json!({
                "resource": format!("mailbox:{ACCOUNT}/{mailbox}"),
                "scope": {"query": "counts"},
            }),
        ));
    }
    assert_eq!(
        queued_loads(&fx.app),
        1,
        "a counts invalidate of any mailbox re-streams, once"
    );
    assert!(fx.app.restream.owed);

    for _ in 0..3 {
        fx.app.apply_event(&event(
            next(),
            "message.row",
            json!({"account": ACCOUNT, "mailbox": "sent", "message": row}),
        ));
    }
    fx.app.apply_event(&event(
        next(),
        "state.remove",
        json!({"resource": format!("message:{ACCOUNT}/archive/9")}),
    ));
    assert_eq!(queued_loads(&fx.app), 1, "one stream in flight");
    assert!(fx.app.restream.owed, "and one owed");

    fx.forget();
    assert_eq!(run_load(&mut fx.app), Some(true));
    assert_eq!(
        fx.methods(),
        ["message.list_conversations", "message.thread"],
        "the expanded conversation is expanded again"
    );
    assert_eq!(fx.app.emails.len(), 7);
    assert_eq!(
        queued_loads(&fx.app),
        1,
        "the owed stream starts when the first lands"
    );
    assert_eq!(run_load(&mut fx.app), Some(true));
    assert_eq!(queued_loads(&fx.app), 0, "nothing more is owed");
    assert_eq!(fx.app.restream.in_flight, None);

    fx.app.apply_event(&event(
        next(),
        "draft.changed",
        json!({"account": ACCOUNT, "id": DRAFT_ID}),
    ));
    assert_eq!(queued_loads(&fx.app), 1, "a draft change re-streams");
    run_load(&mut fx.app);
    fx.app.apply_event(&event(
        next(),
        "state.remove",
        json!({"resource": format!("draft:{ACCOUNT}/{DRAFT_ID}")}),
    ));
    assert_eq!(queued_loads(&fx.app), 1, "a removed draft re-streams");

    // Another account's change is not this list's.
    run_load(&mut fx.app);
    fx.app.apply_event(&event(
        next(),
        "message.row",
        json!({"account": "someone-else", "mailbox": "inbox", "message": row}),
    ));
    fx.app.apply_event(&event(
        next(),
        "state.invalidate",
        json!({"resource": "mailbox:someone-else/inbox", "scope": {"query": "counts"}}),
    ));
    assert_eq!(queued_loads(&fx.app), 0);

    // The flat list keeps its rule: a counts invalidate moves the sidebar and
    // not the list.
    fx.app.set_conversation_mode(false);
    run_load(&mut fx.app);
    fx.app.apply_event(&event(
        next(),
        "state.invalidate",
        json!({"resource": format!("mailbox:{ACCOUNT}/inbox"), "scope": {"query": "counts"}}),
    ));
    assert_eq!(queued_loads(&fx.app), 0);
}

/// The filter matches a conversation on its participants and its latest, and
/// keeps its expanded members with it; flagged-only keeps flagged
/// conversations; jump-to-date lands by the conversation's date.
#[test]
fn filters_and_jumps_run_over_conversation_rows() {
    let mut fx = Fixture::new();
    fx.char('l');

    fx.app.search_query = "ivana".to_string();
    fx.app.apply_search_filter(false);
    assert_eq!(fx.app.visible.len(), 6, "the header and its five members");
    fx.app.search_query = "bot".to_string();
    fx.app.apply_search_filter(false);
    assert_eq!(fx.shape(), ["H1 Wochenbericht"]);
    fx.app.search_query.clear();
    fx.app.apply_search_filter(false);

    fx.forget();
    fx.app.list_index = fx.app.visible.len() - 1;
    fx.dispatch(Action::ToggleFlag);
    fx.app.flagged_only = true;
    fx.app.rebuild_visible();
    assert_eq!(fx.shape(), ["H1 Wochenbericht"]);
    fx.app.flagged_only = false;
    fx.app.rebuild_visible();

    // The conversation is dated by its newest message, the 4th, and its
    // members sit under it whatever their own dates (the question is the
    // 1st), so the first row on or before the 3rd is the lone message of the
    // 2nd.
    fx.app
        .jump_to_date(chrono::NaiveDate::from_ymd_opt(2024, 1, 3).expect("a date"));
    assert_eq!(
        fx.app.selected_email_ref(),
        Some(MessageRef::new(fx.ids.lone))
    );
}

// ---------------------------------------------------------------------------
// A selected conversation
// ---------------------------------------------------------------------------

/// Seed a second conversation of two inbox messages, newer than the first.
fn seed_second_conversation() {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store");
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    let root = "<offer@example.com>";
    let ingest = |uid: i64, id: &str, subject: &str, date: &str, parent: Option<&str>| {
        let raw = parent.map(|p| format!("In-Reply-To: {p}\r\nReferences: {p}\r\n\r\n"));
        crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: ACCOUNT,
                mailbox: "inbox",
                uid,
                email: &message(id, "Carl <carl@example.com>", subject, date, true),
                raw: raw.as_deref().map(str::as_bytes),
            },
        )
        .expect("ingest a fixture message")
        .row_id
    };
    ingest(4, root, "Angebot", "Sat, 6 Jan 2024 08:00:00 +0000", None);
    ingest(
        5,
        "<offer-follow-up@example.com>",
        "Re: Angebot",
        "Sun, 7 Jan 2024 08:00:00 +0000",
        Some(root),
    );
}

/// The row ids in the selection, sorted.
fn selected_rows(app: &App) -> Vec<i64> {
    let mut rows: Vec<i64> = app
        .selection
        .iter()
        .filter_map(EntryKey::msg)
        .map(MessageRef::row_id)
        .collect();
    rows.sort_unstable();
    rows
}

impl Fixture {
    /// Put the cursor on the header whose subject is `subject`.
    fn cursor_on(&mut self, subject: &str) {
        let at = self
            .app
            .visible_emails()
            .position(|e| e.header().is_some() && e.subject == subject);
        self.app.list_index =
            at.unwrap_or_else(|| panic!("a header {subject:?} in {:?}", self.shape()));
    }

    /// Announce a message change in the inbox, as the daemon does, and land
    /// the re-stream it owes.
    fn restream(&mut self, revision: u64) {
        self.app.apply_event(&event(
            revision,
            "state.invalidate",
            json!({
                "resource": format!("mailbox:{ACCOUNT}/inbox"),
                "scope": {"query": "counts"},
            }),
        ));
        assert_eq!(run_load(&mut self.app), Some(true), "the re-stream lands");
    }

    /// Press `a`, confirm, and run the batch the confirmation queues,
    /// answering the rows it archived, sorted.
    fn archive_selection(&mut self) -> Vec<i64> {
        self.char('a');
        self.char('y');
        let at = self
            .app
            .pending_actions
            .iter()
            .position(|a| matches!(a, Action::BatchArchive(_)))
            .expect("a batch archive queued");
        let action = self.app.pending_actions.remove(at).expect("the batch");
        self.forget();
        self.dispatch(action);
        let mut archived = self.rows_of("message.archive");
        archived.sort_unstable();
        archived
    }
}

/// A selected collapsed conversation stands for its members here, and a
/// re-stream, which lists it collapsed again with only its latest as a row
/// key, keeps them all: the batch archive reaches every inbox member.
#[test]
fn a_selected_conversation_keeps_its_members_across_a_restream() {
    let mut fx = Fixture::new();
    let mut inbox = vec![fx.ids.question, fx.ids.follow_up];
    inbox.sort_unstable();
    fx.cursor_on("Re: Frage zum Bericht");
    fx.char('v');
    assert_eq!(selected_rows(&fx.app), inbox);

    fx.restream(WATERMARK + 1);
    assert_eq!(selected_rows(&fx.app), inbox, "the re-stream keeps them");

    assert_eq!(fx.archive_selection(), inbox);
    assert_eq!(fx.shape(), ["H1 Wochenbericht"]);
}

/// Folding another conversation keeps the members of a selected collapsed one,
/// which no row on screen carries but its header.
#[test]
fn a_selected_conversation_keeps_its_members_across_a_collapse() {
    let mut fx = Fixture::new();
    seed_second_conversation();
    fx.restream(WATERMARK + 1);
    assert_eq!(
        fx.shape(),
        [
            "H2 Re: Angebot",
            "H4 Re: Frage zum Bericht",
            "H1 Wochenbericht"
        ]
    );
    let mut inbox = vec![fx.ids.question, fx.ids.follow_up];
    inbox.sort_unstable();
    fx.cursor_on("Re: Frage zum Bericht");
    fx.char('v');
    assert_eq!(selected_rows(&fx.app), inbox);

    fx.cursor_on("Re: Angebot");
    fx.char('l');
    fx.char('j');
    fx.char('h');
    assert_eq!(
        fx.shape(),
        [
            "H2 Re: Angebot",
            "H4 Re: Frage zum Bericht",
            "H1 Wochenbericht"
        ]
    );
    assert_eq!(selected_rows(&fx.app), inbox, "the fold keeps them");

    assert_eq!(fx.archive_selection(), inbox);
    assert_eq!(fx.shape(), ["H2 Re: Angebot", "H1 Wochenbericht"]);
}

/// `tc` and its palette entry switch the account between the conversation list
/// and the flat list, and `tui.json` remembers the choice across a restart
/// (#0142, rollout step 6). The file lives in a config directory of the test's
/// own, never the user's.
#[test]
fn tc_and_the_palette_switch_the_list_and_tui_json_remembers_it() {
    use crate::tui::app::{palette_actions, KeyAction, Overlay};
    use crate::tui::prefs::{tui_prefs_path, ListModes};

    let mut fx = Fixture::new();
    let home = tempfile::tempdir().expect("a home");
    let config = crate::config::test_env::ConfigDirOverride::new(home.path());
    config.set_config_dir(&home.path().join("config"));
    let path = tui_prefs_path();
    assert!(path.starts_with(home.path()), "{}", path.display());
    fx.app.list_modes.path = Some(path.clone());

    fx.char('t');
    fx.char('c');
    assert_eq!(run_load(&mut fx.app), Some(true), "the flat list loads");
    assert!(fx.app.emails.iter().all(|e| e.conv.is_none()), "flat");
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("tui.json written"))
            .expect("tui.json is JSON");
    assert_eq!(stored, json!({ "flat_list_accounts": [ACCOUNT] }));
    assert!(
        fx.app
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("Conversation view off")),
        "{:?}",
        fx.app.status_message
    );

    // A restart reads the same file: the account stays flat, and an account
    // the file does not name is grouped.
    let (restarted, warning) = ListModes::load(path.clone());
    assert_eq!(warning, None);
    assert!(!restarted.grouped(ACCOUNT));
    assert!(restarted.grouped("another"));

    // The palette carries the same row and runs the same toggle.
    let label = palette_actions()
        .into_iter()
        .find(|(action, _)| *action == KeyAction::ToggleConversationMode)
        .map(|(_, label)| label)
        .expect("tc is in the palette");
    fx.char(':');
    for c in label.chars() {
        fx.char(c);
    }
    assert!(matches!(fx.app.overlay, Overlay::Palette(_)));
    fx.key(crossterm::event::KeyCode::Enter);
    assert_eq!(run_load(&mut fx.app), Some(true), "the grouped list loads");
    assert_eq!(fx.shape(), ["H4 Re: Frage zum Bericht", "H1 Wochenbericht"]);
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("tui.json written"))
            .expect("tui.json is JSON");
    assert_eq!(stored, json!({ "flat_list_accounts": [] }));
    assert!(ListModes::load(path).0.grouped(ACCOUNT));
}

/// A `tui.json` that failed to load at start-up is left as it is: `tc` still
/// switches the list, and the status line says the choice was not saved.
#[test]
fn tc_leaves_a_malformed_tui_json_alone_and_says_so() {
    use crate::tui::prefs::{ListModes, TUI_PREFS_FILE};

    let mut fx = Fixture::new();
    let dir = tempfile::tempdir().expect("a config dir");
    let path = dir.path().join(TUI_PREFS_FILE);
    let typo = r#"{"flat_list_accounts": ["work",]}"#;
    std::fs::write(&path, typo).expect("write tui.json");
    let (modes, warning) = ListModes::load(path.clone());
    assert!(warning.is_some());
    fx.app.list_modes = modes;

    fx.char('t');
    fx.char('c');
    assert_eq!(run_load(&mut fx.app), Some(true), "the flat list loads");
    assert!(fx.app.emails.iter().all(|e| e.conv.is_none()), "flat");
    assert_eq!(std::fs::read_to_string(&path).expect("tui.json"), typo);
    assert!(
        fx.app
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("not saved: tui.json is malformed")),
        "{:?}",
        fx.app.status_message
    );
}

/// A message that arrives in a listed conversation, and a reply sent from it,
/// update the row within one re-stream: the counts invalidate the daemon
/// publishes for the mailbox it landed in starts one listing, and that listing
/// is the row with the new member counted, dated and, for the arrival, unread.
#[test]
fn an_arrival_and_a_sent_reply_update_the_row_within_one_restream() {
    let mut fx = Fixture::new();
    let header = |app: &App| app.emails[0].header().cloned().expect("a header");
    let before = header(&fx.app);
    assert_eq!((before.count, before.unread), (4, 2));

    let ingest = |mailbox: &str, uid: i64, email: FetchedEmail, parent: &str| {
        let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store");
        let blobs = crate::store::BlobStore::for_account(ACCOUNT);
        let raw = format!("In-Reply-To: {parent}\r\nReferences: {QUESTION} {parent}\r\n\r\n");
        crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: ACCOUNT,
                mailbox,
                uid,
                email: &email,
                raw: Some(raw.as_bytes()),
            },
        )
        .expect("ingest")
        .row_id
    };
    let invalidate = |app: &mut App, revision: u64, mailbox: &str| {
        app.apply_event(&event(
            revision,
            "state.invalidate",
            json!({
                "resource": format!("mailbox:{ACCOUNT}/{mailbox}"),
                "scope": {"query": "counts"},
            }),
        ));
    };

    let arrival = ingest(
        "inbox",
        6,
        message(
            "<third-question@example.com>",
            "Ivana Petrova <ivana@example.com>",
            "Re: Frage zum Bericht",
            "Sat, 6 Jan 2024 08:00:00 +0000",
            false,
        ),
        SECOND_REPLY,
    );
    invalidate(&mut fx.app, WATERMARK + 1, "inbox");
    assert_eq!(queued_loads(&fx.app), 1, "one re-stream");
    assert_eq!(run_load(&mut fx.app), Some(true));
    let arrived = header(&fx.app);
    assert_eq!((arrived.count, arrived.unread), (5, 3));
    assert!(
        arrived.date_sort.starts_with("2024-01-06"),
        "{}",
        arrived.date_sort
    );
    assert_eq!(arrived.member_ids[0], MessageRef::new(arrival));

    ingest(
        "sent",
        3,
        message(
            "<third-reply@example.com>",
            "Alice Martin <alice@example.com>",
            "Re: Frage zum Bericht",
            "Sun, 7 Jan 2024 09:00:00 +0000",
            true,
        ),
        "<third-question@example.com>",
    );
    invalidate(&mut fx.app, WATERMARK + 2, "sent");
    assert_eq!(queued_loads(&fx.app), 1, "one re-stream");
    assert_eq!(run_load(&mut fx.app), Some(true));
    let replied = header(&fx.app);
    assert_eq!((replied.count, replied.unread), (6, 3));
    assert!(
        replied.date_sort.starts_with("2024-01-07"),
        "{}",
        replied.date_sort
    );
    assert_eq!(
        replied.member_ids, arrived.member_ids,
        "a reply in Sent adds no inbox member"
    );
}
