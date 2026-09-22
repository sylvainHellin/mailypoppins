//! The store-backed half of the list, the counts and the preview memo
//! (#0049 unit 0b, #0124 P5-U4, #0126 P5-U10e).
//!
//! Ported from the file-tree fixtures of #0049 unit 0b and kept in the shape
//! that unit gave them: `parity` means the recorded behaviour must reproduce,
//! and where a `known-bug` case became unreachable the comment says so instead
//! of quietly disappearing.
//!
//! They were `src/tui/app/types.rs`'s own tests until the TUI became a crate
//! that may not link the store. Every one of them seeds a store through the
//! real ingest path and reads it back through [`super::oracle`], so they are
//! the root crate's now; the two that also paint a preview hold a real daemon
//! session, where they used to lean on a fallback the `App` no longer has.

use crate::tui::app::{App, BgResult, MailboxInfo, MailboxKind};

use super::daemon::TestDaemon;
use super::oracle::{count_all_emails, load_emails};

use std::collections::HashSet;

use crate::ingest::{ingest_message, IngestInput};
use crate::parse::FetchedEmail;
use crate::store::{open_store, read, BlobStore, Store};

fn mb(label: &str, id: &str, kind: MailboxKind) -> MailboxInfo {
    MailboxInfo {
        label: label.to_string(),
        icon: "",
        id: id.to_string(),
        kind,
        server_name: None,
    }
}

/// Point the data directory at a temp dir so `config::store_path` and
/// `BlobStore::for_account` resolve inside the fixture. Thread-local, so
/// no other test can observe it (#0077).
struct DataDir {
    _dir: crate::config::test_env::TestDataDir,
}

impl DataDir {
    fn new() -> Self {
        Self {
            _dir: crate::config::test_env::TestDataDir::new(),
        }
    }

    /// Mailbox info whose store key is `name`. No directory is involved
    /// at all: the read path is one query against `messages.mailbox`.
    fn mailbox(&self, label: &str, name: &str, kind: MailboxKind) -> MailboxInfo {
        mb(label, name, kind)
    }
}

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

/// Write a fixture message through the real ingest API, so the rows under
/// test are the rows the sync path actually produces.
fn ingest_fixture(mailbox: &str, uid: i64, email: &FetchedEmail) {
    let store = crate::store::Store::open(crate::config::store_path("alice")).unwrap();
    let blobs = BlobStore::for_account("alice");
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account: "alice",
            mailbox,
            uid,
            email,
            raw: None,
        },
    )
    .unwrap();
}

/// parity. The sidebar number is the number of messages the mailbox holds,
/// and it equals the number of rows `load_emails` produces for the same
/// mailbox. In the file build this was a count of top-level `.md` files
/// and the two could disagree; both sides now read the same rows.
#[test]
fn counts_match_the_number_of_listable_messages() {
    let data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("a", "Mon, 01 Jan 2024 09:00:00 +0000", false));
    ingest_fixture("inbox", 2, &fixture_email("b", "Mon, 01 Jan 2024 10:00:00 +0000", true));
    ingest_fixture("inbox", 3, &fixture_email("c", "Mon, 01 Jan 2024 11:00:00 +0000", true));
    ingest_fixture("archive", 1, &fixture_email("old", "Mon, 01 Jan 2023 09:00:00 +0000", true));

    let mailboxes = vec![
        data.mailbox("Inbox", "inbox", MailboxKind::Inbox),
        data.mailbox("Archive", "archive", MailboxKind::Archive),
    ];

    assert_eq!(count_all_emails("alice", &mailboxes), vec![3, 1]);
    assert_eq!(load_emails("alice", "inbox").len(), 3);
    assert_eq!(load_emails("alice", "archive").len(), 1);
}

/// Write `body` as a draft `.md` into the account's drafts directory, the
/// way an agent or `$EDITOR` does: no id, no index entry, nothing told to
/// the application.
fn external_draft(name: &str, to: &str, subject: &str, status: &str) -> std::path::PathBuf {
    let dir = crate::config::drafts_dir("alice");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("---\nto: {to}\nsubject: {subject}\nstatus: {status}\n---\n\nBody of {subject}\n"),
    )
    .unwrap();
    path
}

/// The Drafts mailbox lists from the drafts index, not from `messages`
/// (#0050 scope item 5). This is the end of the stop-gate state in which
/// the mailbox rendered empty because ingest writes no row for a local
/// draft.
///
/// The entries carry `draft_id` and no `MessageRef`, which is what
/// `Action::CopyMessageRef` and every row-dependent action branch on.
#[test]
fn the_drafts_mailbox_lists_from_the_drafts_index() {
    let _data = DataDir::new();
    external_draft("2026-07-01-note.md", "a@example.com", "Hello", "draft");
    external_draft("2026-07-02-later.md", "b@example.com", "Later", "approved");

    let entries = load_emails("alice", "drafts");
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert!(entry.msg.is_none(), "a draft has no messages row");
        assert_eq!(entry.draft_id.as_ref().map(String::len), Some(16));
        assert!(entry.read, "a draft the user wrote is not unread mail");
    }
    let subjects: HashSet<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
    assert_eq!(subjects, HashSet::from(["Hello", "Later"]));

    // The lister filters nothing: a hand-written `status: sent` file is
    // listed like any other. A draft a *send* retired is gone from the
    // directory, so it is not a status the list has to hide.
    let statuses: HashSet<&str> = entries.iter().map(|e| e.status.as_str()).collect();
    assert_eq!(statuses, HashSet::from(["draft", "approved"]));
    external_draft("2026-07-03-done.md", "c@example.com", "Done", "sent");
    let statuses: HashSet<String> = load_emails("alice", "drafts")
        .iter()
        .map(|e| e.status.clone())
        .collect();
    assert!(statuses.contains("sent"), "{statuses:?}");
    std::fs::remove_file(crate::config::drafts_dir("alice").join("2026-07-03-done.md"))
        .unwrap();

    // And the sidebar agrees with the list it is counting.
    let mailboxes = vec![mb(
        "Drafts",
        crate::selector::DRAFTS_MAILBOX,
        MailboxKind::Drafts,
    )];
    assert_eq!(count_all_emails("alice", &mailboxes), vec![2]);
}

/// A send that reached every recipient retires the draft, so the row
/// leaves the Drafts list, the sidebar count and `mp list`'s query with
/// the file. A partial send keeps all three, marked `sent`, so the retry
/// still has something to name.
#[test]
fn a_fully_sent_draft_leaves_the_drafts_list_and_a_partial_one_stays() {
    let _data = DataDir::new();
    let done = external_draft("2026-07-01-note.md", "a@example.com", "Hello", "approved");
    let partial = external_draft("2026-07-02-later.md", "b@example.com", "Later", "approved");
    assert_eq!(load_emails("alice", "drafts").len(), 2);

    let outcome = |ok: bool| crate::send::SendReport {
        send_result: crate::send::SendResult {
            results: vec![crate::send::RecipientResult {
                address: "a@example.com".to_string(),
                role: crate::send::RecipientRole::To,
                success: ok,
                error: None,
                verdict: if ok {
                    crate::send::RecipientVerdict::Delivered
                } else {
                    crate::send::RecipientVerdict::Rejected
                },
            }],
        },
        state: Some(crate::outbox::OutboxState::Done),
        row_id: Some(1),
    };
    let settle = |path: &std::path::Path, ok: bool| {
        let draft = crate::draft::parse_email_draft(path).unwrap();
        crate::draft::settle_sent_draft(&draft, &outcome(ok), None).unwrap();
    };
    settle(&done, true);
    settle(&partial, false);

    assert!(!done.exists(), "the fully sent draft left drafts/");
    let entries = load_emails("alice", "drafts");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject, "Later");
    assert_eq!(entries[0].status, "sent", "a partial send stays addressable");
    assert_eq!(
        count_all_emails(
            "alice",
            &[mb("Drafts", crate::selector::DRAFTS_MAILBOX, MailboxKind::Drafts)]
        ),
        vec![1]
    );

    // And `mp list` reads the same table the list just refreshed.
    let store = Store::open(crate::config::store_path("alice")).unwrap();
    let rows = crate::store::drafts::list(&store, "alice", None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject.as_deref(), Some("Later"));
}

/// The sidebar count and the Drafts list read the same index through the
/// same open, so an account that has never synced (no store file yet, its
/// drafts written straight into the directory) cannot list drafts and
/// count 0 beside them. Nothing has loaded the mailbox here: the count is
/// the first read.
#[test]
fn the_drafts_count_agrees_with_the_list_on_a_never_synced_account() {
    let _data = DataDir::new();
    external_draft("2026-08-01-one.md", "a@example.com", "One", "draft");
    external_draft("2026-08-02-two.md", "b@example.com", "Two", "draft");
    assert!(
        !crate::config::store_path("alice").exists(),
        "the account must be un-synced for this to be the case under test"
    );

    let mailboxes = vec![mb(
        "Drafts",
        crate::selector::DRAFTS_MAILBOX,
        MailboxKind::Drafts,
    )];
    assert_eq!(count_all_emails("alice", &mailboxes), vec![2]);
    assert_eq!(load_emails("alice", "drafts").len(), 2);
}

/// The [TKT-0045] scenario from the TUI's side: a draft written by another
/// process is listed by the next load, with no restart and no `mp`
/// command in between, because the load refreshes the index itself. The
/// one-second [`crate::store::drafts::fingerprint`] poll in the event loop
/// is what asks for that load.
#[test]
fn a_draft_written_externally_appears_on_the_next_load() {
    let _data = DataDir::new();
    external_draft("first.md", "a@example.com", "First", "draft");
    let before = load_emails("alice", "drafts");
    assert_eq!(before.len(), 1);
    let fingerprint_before =
        crate::store::drafts::fingerprint(&crate::config::drafts_dir("alice"));

    external_draft("second.md", "b@example.com", "Second", "draft");

    assert_ne!(
        fingerprint_before,
        crate::store::drafts::fingerprint(&crate::config::drafts_dir("alice")),
        "the poll must see the new file"
    );
    let after = load_emails("alice", "drafts");
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|e| e.subject == "Second"));
}

/// parity. A mailbox that was configured but never synced counts 0 rather
/// than being skipped, so the returned vector stays index-aligned with
/// `mailboxes`. An account with no store at all is the same case.
#[test]
fn counts_are_zero_for_unsynced_mailboxes_and_stay_index_aligned() {
    let data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("a", "Mon, 01 Jan 2024 09:00:00 +0000", false));

    let mailboxes = vec![
        data.mailbox("Never synced", "some-folder", MailboxKind::Extra),
        data.mailbox("Inbox", "inbox", MailboxKind::Inbox),
        data.mailbox("Sent", "sent", MailboxKind::Sent),
    ];
    assert_eq!(count_all_emails("alice", &mailboxes), vec![0, 1, 0]);
    assert_eq!(count_all_emails("alice", &[]), Vec::<usize>::new());

    // No store file yet: every count is zero and nothing panics.
    assert_eq!(count_all_emails("nobody", &mailboxes), vec![0, 0, 0]);
    assert!(load_emails("nobody", "inbox").is_empty());
}

/// parity. The count is a total, not an unread count: it is identical for
/// an all-read and an all-unread mailbox. There was no unread-count
/// function in the file build and there is none here, so the sidebar still
/// cannot show one (#0049 recorded this as a gap, not a contract).
#[test]
fn counts_ignore_the_read_flag() {
    let data = DataDir::new();
    for i in 0..3 {
        ingest_fixture(
            "inbox",
            i + 1,
            &fixture_email(&format!("r{i}"), "Mon, 01 Jan 2024 09:00:00 +0000", true),
        );
        ingest_fixture(
            "archive",
            i + 1,
            &fixture_email(&format!("u{i}"), "Mon, 01 Jan 2024 09:00:00 +0000", false),
        );
    }

    let mailboxes = vec![
        data.mailbox("Read", "inbox", MailboxKind::Inbox),
        data.mailbox("Unread", "archive", MailboxKind::Archive),
    ];
    assert_eq!(count_all_emails("alice", &mailboxes), vec![3, 3]);
}

/// Resolution of the `known-bug` case recorded in #0049 unit 0b
/// (`count_all_emails_counts_files_the_list_cannot_show`): the file build
/// counted a non-UTF-8 `.md` that `load_emails` then dropped, so the
/// sidebar said 2 while the list showed 1.
///
/// The bug is gone by construction, not by fix: there is no file to be
/// unreadable, and both numbers come from the same rows. The honest
/// store-side statement of the same property is that an unreadable *body
/// blob* (the nearest surviving analogue, and a case retention can
/// genuinely produce) still lists and still counts, because the envelope
/// lives in the row. It degrades to an empty body, never to a missing row.
#[test]
fn a_message_whose_body_blob_is_gone_still_lists_and_still_counts() {
    let data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("good", "Mon, 01 Jan 2024 09:00:00 +0000", false));
    ingest_fixture("inbox", 2, &fixture_email("broken", "Mon, 01 Jan 2024 10:00:00 +0000", false));

    // Evict one body the way a retention sweep would: unlink the blob file
    // and leave the row pointing at it.
    let store = crate::store::Store::open(crate::config::store_path("alice")).unwrap();
    let hash: String = store
        .conn()
        .query_row(
            "SELECT body_blob FROM messages WHERE subject = 'broken'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let blobs = BlobStore::for_account("alice");
    std::fs::remove_file(blobs.path_for(&crate::store::BlobHash::parse(&hash).unwrap()))
        .unwrap();
    drop(store);

    let mailboxes = vec![data.mailbox("Inbox", "inbox", MailboxKind::Inbox)];
    let entries = load_emails("alice", "inbox");
    assert_eq!(entries.len(), 2, "the row survives its blob");
    assert_eq!(count_all_emails("alice", &mailboxes), vec![2]);
    let broken = entries.iter().find(|e| e.subject == "broken").unwrap();
    assert_eq!(broken.from, "Sender broken", "the envelope is intact");

    // The body is no longer part of the entry, so the eviction is only
    // visible where the body is actually wanted: the preview's on-demand
    // read degrades to an empty body, one message wide.
    let store = open_store("alice").unwrap();
    let blobs = BlobStore::for_account("alice");
    assert_eq!(
        read::load_body(&store, &blobs, broken.msg.unwrap().row_id()).unwrap(),
        "",
        "an evicted body degrades to empty"
    );
    let good = entries.iter().find(|e| e.subject == "good").unwrap();
    assert_eq!(
        read::load_body(&store, &blobs, good.msg.unwrap().row_id()).unwrap(),
        "body of good",
        "the neighbouring body is untouched"
    );
}

/// parity. The list is newest first, exactly as the file build sorted it,
/// and the sort now happens in SQL over `date_sort`.
#[test]
fn the_list_is_newest_first_and_deterministic() {
    let _data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("older", "Mon, 01 Jan 2024 09:00:00 +0000", false));
    ingest_fixture("inbox", 2, &fixture_email("newest", "Mon, 01 Jan 2024 18:00:00 +0000", false));
    ingest_fixture("inbox", 3, &fixture_email("middle", "Mon, 01 Jan 2024 12:00:00 +0000", false));

    let subjects: Vec<String> = load_emails("alice", "inbox")
        .into_iter()
        .map(|e| e.subject)
        .collect();
    assert_eq!(subjects, vec!["newest", "middle", "older"]);
    let again: Vec<String> = load_emails("alice", "inbox")
        .into_iter()
        .map(|e| e.subject)
        .collect();
    assert_eq!(subjects, again, "two loads must agree");
}

/// parity. Display fields keep the file build's rules: the display name is
/// extracted from the address, the date is the sender-local day with a UTC
/// sort key, and an empty subject becomes the `(no subject)` placeholder.
#[test]
fn display_fields_follow_the_file_builds_rules() {
    let _data = DataDir::new();
    let mut e = fixture_email("x", "Mon, 06 May 2024 10:00:00 +0200", true);
    e.subject = String::new();
    e.from = "Ada Lovelace <ada@example.com>".into();
    ingest_fixture("inbox", 1, &e);

    let entries = load_emails("alice", "inbox");
    let entry = &entries[0];
    assert_eq!(entry.subject, "(no subject)");
    assert_eq!(entry.from, "Ada Lovelace");
    assert_eq!(entry.date_display, "2024-05-06", "display stays sender-local");
    assert_eq!(entry.date_sort, "2024-05-06T08:00:00", "the sort key is UTC");
    assert!(entry.read);
    assert_eq!(entry.status, "inbox", "status comes from the mailbox now");
}

// -----------------------------------------------------------------------
// Lazy bodies (#0038 scope item 5)
// -----------------------------------------------------------------------

/// An app parked on one store-backed mailbox, as `App::new` would leave it.
fn app_on_inbox() -> App {
    let mut app = App::default_for_tests();
    app.account_config.name = "alice".to_string();
    // The preview reads through the daemon since P5-U4 and through nothing
    // else since P5-U10e, so the fixture holds a session where it used to
    // lean on the store-backed fallback the `App` no longer has.
    app.session = Some(TestDaemon::new(&["alice"]).session());
    app.mailboxes = vec![mb(
        "Inbox",
        "inbox",
        MailboxKind::Inbox,
    )];
    app.mailbox_counts = vec![0];
    app.email_cache = vec![None];
    app.emails = std::sync::Arc::new(load_emails("alice", "inbox"));
    app.email_cache[0] = Some(std::sync::Arc::clone(&app.emails));
    app.rebuild_visible();
    app
}

/// The list load itself reads no blob at all, which is the cold-start
/// criterion: every body blob can be missing and the mailbox still lists,
/// counts and displays. Only a body actually asked for degrades, one
/// message wide.
#[test]
fn the_list_loads_with_every_body_blob_missing() {
    let data = DataDir::new();
    for i in 1..=3 {
        ingest_fixture(
            "inbox",
            i,
            &fixture_email(&format!("m{i}"), "Mon, 01 Jan 2024 09:00:00 +0000", false),
        );
    }
    std::fs::remove_dir_all(BlobStore::for_account("alice").root()).unwrap();

    let entries = load_emails("alice", "inbox");
    assert_eq!(entries.len(), 3, "the rows are all the list needs");
    assert!(entries.iter().all(|e| e.subject.starts_with('m')));
    assert_eq!(
        count_all_emails("alice", &[data.mailbox("Inbox", "inbox", MailboxKind::Inbox)]),
        vec![3]
    );

    let mut app = app_on_inbox();
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "", "only the preview degrades");
}

/// The preview loads the body of the message the cursor is on, and follows
/// the cursor. Moving to another message reads that message's blob.
#[test]
fn the_preview_loads_the_body_of_the_selected_message() {
    let _data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("newest", "Mon, 01 Jan 2024 12:00:00 +0000", false));
    ingest_fixture("inbox", 2, &fixture_email("older", "Mon, 01 Jan 2024 09:00:00 +0000", false));

    let mut app = app_on_inbox();
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "body of newest");

    app.list_index = 1;
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "body of older");

    // A frame that changes nothing re-reads nothing, and shows the same
    // body: the memo answers from its key.
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "body of older");
}

/// The preview query carries its own timing span, entered once per body
/// build and never on a memo hit, so `[TIMING] tui_preview_query` in the
/// log counts store reads rather than frames (#0118 P0-U5).
///
/// This host has no configured account, so the span cannot be observed in
/// a real run; the counter the span constructor bumps stands in for the
/// log line.
#[test]
fn the_preview_query_span_is_entered_once_per_body_build() {
    let _data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("newest", "Mon, 01 Jan 2024 12:00:00 +0000", false));
    ingest_fixture("inbox", 2, &fixture_email("older", "Mon, 01 Jan 2024 09:00:00 +0000", false));

    let spans = || crate::tui::app::PREVIEW_QUERY_SPANS.with(|n| n.get());
    let mut app = app_on_inbox();
    let before = spans();

    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "body of newest");
    assert_eq!(spans() - before, 1, "the first paint on a row is one query");

    app.refresh_preview_body();
    app.refresh_preview_body();
    assert_eq!(spans() - before, 1, "a memo hit reads nothing and times nothing");

    app.list_index = 1;
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "body of older");
    assert_eq!(spans() - before, 2, "one cursor move, one span");
}

/// A re-ingest that rewrites the body reaches the preview, because the
/// reload that publishes the new rows bumps the generation the memo is
/// keyed by. This is the case a body parked in `EmailEntry` could only
/// answer by cloning the whole shared list.
#[test]
fn the_preview_body_follows_a_reingest() {
    let _data = DataDir::new();
    ingest_fixture("inbox", 1, &fixture_email("subject", "Mon, 01 Jan 2024 12:00:00 +0000", false));

    let mut app = app_on_inbox();
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "body of subject");

    let mut rewritten = fixture_email("subject", "Mon, 01 Jan 2024 12:00:00 +0000", false);
    rewritten.body_text = "a corrected body".to_string();
    ingest_fixture("inbox", 1, &rewritten);

    // The reload path the TUI takes after a sync: invalidate, request
    // (which bumps the generation), then deliver off the background thread.
    app.reload_current_mailbox();
    let loaded = BgResult::MailboxLoaded {
        account_index: app.active_account,
        mailbox_idx: app.active_mailbox,
        generation: app.mailbox_load_generation,
        entries: load_emails("alice", "inbox"),
    };
    crate::tui::bg::handle_bg_result(&mut app, loaded);

    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "a corrected body");
}

/// An app parked on the Drafts mailbox, which lists from the drafts index
/// rather than from `messages`.
fn app_on_drafts() -> App {
    let mut app = App::default_for_tests();
    app.account_config.name = "alice".to_string();
    app.session = Some(TestDaemon::new(&["alice"]).session());
    app.mailboxes = vec![mb(
        "Drafts",
        crate::selector::DRAFTS_MAILBOX,
        MailboxKind::Drafts,
    )];
    app.mailbox_counts = vec![0];
    app.email_cache = vec![None];
    app.emails = std::sync::Arc::new(load_emails("alice", "drafts"));
    app.email_cache[0] = Some(std::sync::Arc::clone(&app.emails));
    app.rebuild_visible();
    app
}

/// Park the cursor on the draft whose subject is `subject`, whatever order
/// the index listed the directory in.
fn cursor_on(app: &mut App, subject: &str) {
    app.list_index = app
        .visible
        .iter()
        .position(|&i| app.emails[i].subject == subject)
        .unwrap_or_else(|| panic!("no draft row for {subject}"));
}

/// The Body pane of a draft row shows the draft's own markdown. It was
/// blank for every draft, because the memo was keyed on a `MessageRef` and
/// a draft has none: the key never built, so the pane never filled.
#[test]
fn the_preview_shows_the_body_of_the_selected_draft() {
    let _data = DataDir::new();
    external_draft("2026-07-01-note.md", "a@example.com", "Hello", "draft");
    external_draft("2026-07-02-later.md", "b@example.com", "Later", "approved");

    let mut app = app_on_drafts();
    cursor_on(&mut app, "Hello");
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "Body of Hello");

    // The memo follows the cursor across drafts, one file read per move.
    cursor_on(&mut app, "Later");
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "Body of Later");

    // A frame that changes nothing re-reads nothing and answers the same.
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "Body of Later");

    // A draft carries no ics blob, so the invite card stays empty rather
    // than reaching for a store row that does not exist.
    app.refresh_preview_invite();
    assert!(app.preview_invite.event().is_none());
}

/// The index can name a file that is no longer there: another process moved
/// it, or a send retired it between the poll and the frame. The preview
/// degrades to an empty pane, one draft wide, and does not panic.
#[test]
fn a_draft_whose_file_vanished_previews_empty() {
    let _data = DataDir::new();
    let path = external_draft("2026-07-01-note.md", "a@example.com", "Hello", "draft");

    let mut app = app_on_drafts();
    cursor_on(&mut app, "Hello");
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "Body of Hello");

    // The row survives in the list (it was loaded before), the file does
    // not, and the generation bump is what asks the memo again.
    std::fs::remove_file(&path).unwrap();
    app.mailbox_load_generation += 1;
    app.refresh_preview_body();
    assert_eq!(app.preview_body.text(), "");
}

/// #0088: the in-list metadata filter (the old `/`, now `fm`) is served
/// from the loaded rows only. Body search is retired from this path (the
/// old `\`, whose synchronous bulk blob read froze the UI); a body-only
/// token no longer narrows the list, while the sender does, which the old
/// `/` could not answer (UX audit §b.5). The unified `ff` entry is where
/// body search now lives, served off the UI thread.
#[test]
fn the_in_list_filter_matches_the_sender_but_no_longer_the_body() {
    let _data = DataDir::new();
    // A token that lives only in the body, in no header field.
    let mut buried = fixture_email("Trip", "Mon, 01 Jan 2024 09:00:00 +0000", false);
    buried.body_text = "let us go kayaking".to_string();
    buried.from = "trips@example.com".to_string();
    ingest_fixture("inbox", 1, &buried);
    // A distinctive sender.
    let mut outbound = fixture_email("Numbers", "Mon, 01 Jan 2024 10:00:00 +0000", false);
    outbound.from = "cfo@acme.example".to_string();
    ingest_fixture("inbox", 2, &outbound);

    let mut app = app_on_inbox();
    let subjects = |app: &crate::tui::app::App| -> Vec<String> {
        app.visible_emails().map(|e| e.subject.clone()).collect()
    };

    // The retired `\` body search is gone: a body-only token does not
    // narrow the list.
    app.search_query = "kayaking".to_string();
    app.apply_search_filter(false);
    assert!(
        subjects(&app).is_empty(),
        "body substrings no longer match in the in-list filter"
    );

    // Searching by sender lands.
    app.search_query = "cfo".to_string();
    app.apply_search_filter(false);
    assert_eq!(subjects(&app), vec!["Numbers"]);
}
