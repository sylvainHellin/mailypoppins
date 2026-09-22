//! The command layer over a real daemon (P5-U6, #0124).
//!
//! Every row here drives `crate::tui::commands` against the in-process
//! dispatcher of [`super::daemon`] over a seeded store, which is what makes it
//! the root crate's test and not `src/tui/commands.rs`'s own since #0126
//! (P5-U10e): a client crate cannot link the daemon it talks to.

use serde_json::{json, Value};

use crate::selector::Selector;
use crate::tui::app::{App, EmailEntry, MessageRef};
use crate::tui::commands::*;
use crate::tui::events::Awaited;
use crate::tui::queries::Queries;

use super::daemon::TestDaemon;

const ACCOUNT: &str = "alice";

/// A daemon over a fixture data root, reachable as a [`Queries`].
///
/// [`TestDaemon`](super::daemon::TestDaemon) is the shared
/// fixture; what is added here is the seeded account it serves and the
/// tempdir that holds it, which lives as long as the fixture because every
/// path in sight resolves under it.
struct Daemon {
    daemon: TestDaemon,
    _data: crate::config::test_env::TestDataDir,
}

impl Daemon {
    fn new() -> Daemon {
        let data = crate::config::test_env::TestDataDir::new();
        std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
        drop(crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store"));
        Daemon {
            daemon: TestDaemon::new(&[ACCOUNT]),
            _data: data,
        }
    }
}

impl Queries for Daemon {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.daemon.call(method, params)
    }
}

/// One list row, everything about it derived from `subject`.
fn entry(subject: &str, id: i64, is_invite: bool) -> EmailEntry {
    EmailEntry {
        msg: Some(MessageRef::new(id)),
        draft_id: None,
        skip: None,
        selector: None,
        from: "Sender <s@example.com>".to_string(),
        to: "me@example.com".to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.to_string(),
        status: "inbox".to_string(),
        date_display: "2026-07-01".to_string(),
        date_sort: "2026-07-01T00:00:00".to_string(),
        has_attachments: false,
        read: false,
        answered: false,
        forwarded: false,
        flagged: false,
        is_invite,
    }
}

/// One drafts row under the cursor, in the state the file is in.
fn draft_entry(id: &str, status: &str) -> EmailEntry {
    EmailEntry {
        msg: None,
        draft_id: Some(id.to_string()),
        skip: None,
        selector: None,
        from: String::new(),
        to: "alice@example.com".to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: "Re: Hello".to_string(),
        status: status.to_string(),
        date_display: "2026-07-01".to_string(),
        date_sort: "2026-07-01T00:00:00".to_string(),
        has_attachments: false,
        read: true,
        answered: false,
        forwarded: false,
        flagged: false,
        is_invite: false,
    }
}

/// An app on `ACCOUNT` whose cursor sits on `id`'s Drafts row.
fn app_on_draft(id: &str, status: &str) -> App {
    let mut app = App::default_for_tests();
    app.account_config.name = ACCOUNT.to_string();
    app.emails = std::sync::Arc::new(vec![draft_entry(id, status)]);
    app.rebuild_visible();
    app
}

/// Write one draft file and index it, handing back its id.
fn a_draft(id: &str) -> String {
    let dir = crate::config::drafts_dir(ACCOUNT);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.md")),
        format!(
            "---\nid: {id}\nfrom: me@example.com\nto: you@example.com\nsubject: Hello\n\
             status: draft\ndate: 2024-01-01T09:00:00+00:00\n---\n\nBody.\n"
        ),
    )
    .unwrap();
    crate::store::drafts::refresh_account(ACCOUNT).unwrap();
    id.to_string()
}

/// The mark that rides on an explicit open (#0110) is a real mutation, not
/// an intent: the store row gains `\Seen` and exactly one `SetRead` op is
/// owed to the server, because `message.set_read` queues the pair (#0039).
/// A second call over the same row is a no-op, so re-opening does not queue
/// a duplicate op.
///
/// It was `actions.rs`'s until P5-U6 moved the mutation here; what is new
/// is the door, and that the row is written by the daemon over an account
/// with no credentials, which only a queueing call can do.
#[test]
fn an_open_marks_the_row_it_resolved_and_queues_one_server_op() {
    let daemon = Daemon::new();
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    let email = crate::parse::FetchedEmail {
        from: "Sender <s@example.com>".into(),
        to: "me@example.com".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: "Unread".into(),
        date: "Mon, 20 Jul 2026 09:00:00 +0000".into(),
        body_text: "Hello.".into(),
        html_body: None,
        has_attachments: false,
        message_id: Some("<inbox-1@example.com>".into()),
        attachments: Vec::new(),
        flags: Default::default(),
        calendar_ics: None,
        event: None,
    };
    let row_id = crate::ingest::ingest_message(
        &store,
        &blobs,
        &crate::ingest::IngestInput {
            account: ACCOUNT,
            mailbox: "inbox",
            uid: 1,
            email: &email,
            raw: None,
        },
    )
    .unwrap()
    .row_id;
    drop(store);

    let mut app = App::default_for_tests();
    app.account_config.name = ACCOUNT.to_string();
    // The cursor sits on a *different* row than the one that was opened,
    // which is what a `Tab` and a `J` coalesced into one batch produce
    // (#0108): the mark must follow the ref it was given, not the cursor.
    app.emails = std::sync::Arc::new(vec![
        entry("Unread", row_id, false),
        entry("Moved onto", row_id + 1, false),
    ]);
    app.visible = vec![0, 1];
    app.list_index = 1;

    let msg = MessageRef::new(row_id);
    assert!(
        mark_open_read(&mut app, &daemon, msg),
        "the open marked nothing"
    );
    assert!(app.emails[0].read, "the opened list row is stale");
    assert!(!app.emails[1].read, "the row under the cursor was marked");

    let store = crate::store::open_store(ACCOUNT).unwrap();
    assert!(crate::store::read::find_by_id(&store, row_id)
        .unwrap()
        .unwrap()
        .is_read());
    let queued = crate::pending_ops::queued_ops(&store, ACCOUNT).unwrap();
    assert_eq!(queued.len(), 1, "expected exactly one owed server op");
    assert_eq!(
        queued[0].op,
        crate::ops::ServerOp::SetRead {
            message_id: "<inbox-1@example.com>".to_string(),
            // The row's own mailbox as `find_server_name_for_role` spells
            // it, which for an account with no `[[mailboxes]]` mapping is
            // the role verbatim. It is the daemon's spelling since P4-U8
            // and it addresses the row's mailbox rather than the open one.
            mailbox: "inbox".to_string(),
            read: true,
        }
    );
    drop(store);

    assert!(
        !mark_open_read(&mut app, &daemon, msg),
        "an already-read row re-marked"
    );
    let store = crate::store::open_store(ACCOUNT).unwrap();
    assert_eq!(
        crate::pending_ops::queued_ops(&store, ACCOUNT)
            .unwrap()
            .len(),
        1,
        "re-opening queued a duplicate op"
    );
}

/// The agenda is only rebuilt when a mutation actually touched an invite,
/// which is read off the list rows *before* they are removed.
#[test]
fn only_a_mutation_that_touches_an_invite_asks_for_an_agenda_rebuild() {
    let mut app = App::default_for_tests();
    app.emails =
        std::sync::Arc::new(vec![entry("Standup", 1, true), entry("Receipt", 2, false)]);

    assert!(any_invite(&app, &[MessageRef::new(1)]));
    assert!(any_invite(&app, &[MessageRef::new(2), MessageRef::new(1)]));
    assert!(!any_invite(&app, &[MessageRef::new(2)]));
    assert!(!any_invite(&app, &[MessageRef::new(404)]));
}

/// Approve and mark-draft flip the file `mp mark-approved` /
/// `mp mark-draft` flip, name the draft by its selector, and leave the file
/// holding the new status.
///
/// "Already approved" is read off the row the user is looking at rather
/// than off the library's return sentence, which the daemon does not carry:
/// the list column and the status line therefore cannot disagree.
#[test]
fn approve_and_mark_draft_flip_the_indexed_status() {
    let daemon = Daemon::new();
    let id = a_draft("one");
    let selector = Selector::for_draft(ACCOUNT, &id);
    let mut app = app_on_draft(&id, "draft");

    status_flip(&mut app, &daemon, Flip::Approve);
    assert_eq!(
        app.status_message.as_deref(),
        Some(&*format!("Approved {selector}"))
    );
    assert!(draft_file_says(&id, "status: approved"));

    // The reload the flip triggers emptied the list (the fixture app has
    // no session to load from), so the cursor is put back by hand, on the
    // row as the flip left it.
    app.emails = std::sync::Arc::new(vec![draft_entry(&id, "approved")]);
    app.rebuild_visible();
    status_flip(&mut app, &daemon, Flip::Approve);
    assert_eq!(
        app.status_message.as_deref(),
        Some(&*format!("Already approved: {selector}"))
    );

    app.emails = std::sync::Arc::new(vec![draft_entry(&id, "approved")]);
    app.rebuild_visible();
    status_flip(&mut app, &daemon, Flip::Demote);
    assert_eq!(
        app.status_message.as_deref(),
        Some(&*format!("Demoted {selector}"))
    );
    assert!(draft_file_says(&id, "status: draft"));
}

/// An illegal transition fails with the daemon's own error text, which is
/// the sentence `mp mark-draft` prints: a sent email has left the draft
/// pipeline and is not rewritten back into it.
#[test]
fn marking_a_sent_draft_back_to_draft_fails_like_the_cli() {
    let daemon = Daemon::new();
    let id = a_draft("one");
    let dir = crate::config::drafts_dir(ACCOUNT);
    let path = dir.join("one.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("status: draft", "status: sent")).unwrap();
    crate::store::drafts::refresh_account(ACCOUNT).unwrap();

    let mut app = app_on_draft(&id, "sent");
    status_flip(&mut app, &daemon, Flip::Demote);

    let status = app.status_message.clone().unwrap();
    assert!(
        status.starts_with("Mark-draft failed:")
            && status.contains("Cannot revert a sent email back to draft"),
        "{status}"
    );
}

/// The batch flips every selected draft and counts what it could not do,
/// which is the pre-nuke build's contract: one refusal is one failure, not
/// an abort.
#[test]
fn the_batch_flips_every_selected_draft_and_counts_the_refusals() {
    let daemon = Daemon::new();
    let one = a_draft("one");
    let two = a_draft("two");
    let mut app = App::default_for_tests();
    app.account_config.name = ACCOUNT.to_string();

    status_flip_batch(
        &mut app,
        &daemon,
        &[one.clone(), two.clone()],
        Flip::Approve,
    );
    assert_eq!(app.status_message.as_deref(), Some("Approved 2 drafts"));
    assert!(draft_file_says(&one, "status: approved"));
    assert!(draft_file_says(&two, "status: approved"));

    status_flip_batch(
        &mut app,
        &daemon,
        &[one.clone(), "not-in-the-index".to_string()],
        Flip::Demote,
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Marked 1/2 as draft (1 failed)")
    );
    assert!(draft_file_says(&one, "status: draft"));
    assert!(draft_file_says(&two, "status: approved"));
}

// -----------------------------------------------------------------------
// The operations, started here and finished by event (P5-U8)
// -----------------------------------------------------------------------

/// A started operation is remembered by id and counted as background
/// work, and its finished event lands as the `BgResult` its arm posted.
///
/// `calendar.rebuild` is the cheapest real operation the fixture can run:
/// it folds the stored replies onto the stored invitations of an account
/// with neither, so what is pinned is the machinery every sync,
/// send-approved and RSVP arm rides on, not the fold. The finish itself
/// arrives over a socket in a real run (`tests/tui_daemon_recovery.rs`),
/// so here the settled payload is handed to [`settled`] directly.
#[test]
fn a_started_operation_is_remembered_until_its_finish_lands() {
    let daemon = Daemon::new();
    let mut app = App::default_for_tests();

    assert!(start_operation(
        &mut app,
        &daemon,
        "calendar.rebuild",
        json!({"account": ACCOUNT}),
        Awaited::Quick {
            account_index: 0,
            account: ACCOUNT.to_string(),
        },
    ));
    assert_eq!(app.bg_count, 1, "an operation is background work");

    let landed = settled(
        &Awaited::Quick {
            account_index: 0,
            account: ACCOUNT.to_string(),
        },
        &json!({
            "operation_id": "whatever",
            "state": "succeeded",
            "result": {"blocked": false, "outcome": {
                "account": ACCOUNT, "severity": "ok", "saved": 1, "skipped": 2,
                "flags_updated": 0, "pruned": 0, "prunes_deferred": 0, "uid_rebound": 0,
                "uidvalidity_resets": 0, "bodies_truncated": 0, "non_converging": [],
                "failed_mutations": 0, "error": null, "new_inbox_mail": [],
            }},
        }),
    );
    match landed {
        crate::tui::app::BgResult::Fetch { result, .. } => assert_eq!(
            result.expect("a settled pass"),
            "Synced: 1 new, 2 existing",
            "the line the polled answer produced, from the same pure function"
        ),
        other => panic!("a quick pass lands as a Fetch, got {other:?}"),
    }
}

/// A refused operation never starts, and the daemon's own sentence is what
/// the status line says.
///
/// An account with no server configured is refused by `sync.quick` before
/// an operation is created at all, which is the branch a sync over a
/// local-only account takes.
#[test]
fn a_refused_sync_is_the_sentence_the_daemon_gave() {
    let daemon = Daemon::new();
    let mut app = App::default_for_tests();

    assert!(
        !start_operation(
            &mut app,
            &daemon,
            "sync.quick",
            json!({"account": ACCOUNT}),
            Awaited::Quick {
                account_index: 0,
                account: ACCOUNT.to_string(),
            },
        ),
        "an account with no server has nothing to sync"
    );
    let line = app.status_message.clone().expect("a refusal is shown");
    assert!(
        line.contains("configures no server"),
        "the daemon's own refusal, verbatim: {line}"
    );
    assert_eq!(app.bg_count, 0, "nothing started, so nothing is pending");
}

/// The local search pass answers the rows the index holds, addressed by
/// the query rendered back into the grammar `message.search` parses.
///
/// The round trip is what this pins beyond `search::to_query_string`'s own
/// tests: the overlay's AST, rendered, sent, re-parsed daemon-side and run
/// against the FTS index, finds the row a store-backed `search_ast` found.
/// The body travels with the hit, because the overlay renders it from the
/// `fetched` payload rather than from a second read.
#[test]
fn the_local_pass_finds_the_row_the_index_holds() {
    let daemon = Daemon::new();
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    let email = crate::parse::FetchedEmail {
        from: "Sender <s@example.com>".into(),
        to: "me@example.com".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: "Quarterly zolvertrix".into(),
        date: "Mon, 20 Jul 2026 09:00:00 +0000".into(),
        body_text: "The zolvertrix is in the ledger.".into(),
        html_body: None,
        has_attachments: false,
        message_id: Some("<hit-1@example.com>".into()),
        attachments: Vec::new(),
        flags: Default::default(),
        calendar_ics: None,
        event: None,
    };
    crate::ingest::ingest_message(
        &store,
        &blobs,
        &crate::ingest::IngestInput {
            account: ACCOUNT,
            mailbox: "inbox",
            uid: 1,
            email: &email,
            raw: None,
        },
    )
    .unwrap();
    drop(store);

    let query = crate::search::parse("zolvertrix").unwrap();
    let hits = local_search(&daemon, ACCOUNT, &query, Some("inbox"), 50);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].entry.subject, "Quarterly zolvertrix");
    assert_eq!(hits[0].source_label, "inbox");
    assert!(
        hits[0].fetched.body_text.contains("zolvertrix"),
        "the hit carries its body: {:?}",
        hits[0].fetched.body_text
    );

    // A query nothing matches is an empty pass, not a failure: the server
    // leg is what answers next either way.
    let miss = crate::search::parse("nothingmatchesthis").unwrap();
    assert!(local_search(&daemon, ACCOUNT, &miss, None, 50).is_empty());
}

/// The three sentences a finished `send.draft` shows, which are the ones
/// the send key has posted since #0037.
///
/// They were built from the engine's own `SendReport` until P6-U2 moved
/// the send behind `send.draft`; they are built from the `SendOutcome` the
/// daemon settles with now, and they may not have been reworded on the
/// way. A send nobody took is an `Err`, because the message is parked in
/// the outbox for a human rather than gone.
#[test]
fn the_send_lines_are_the_ones_the_send_key_has_always_shown() {
    let outcome = |delivered: &[bool]| {
        json!({
            "account": ACCOUNT,
            "selector": null,
            "message_id": "<x@example.com>",
            "status_line": "queued for delivery",
            "recipients": delivered
                .iter()
                .enumerate()
                .map(|(at, ok)| json!({
                    "address": format!("r{at}@example.com"),
                    "role": "To",
                    "delivered": ok,
                    "error": null,
                }))
                .collect::<Vec<_>>(),
            "sent_copy": "pending",
            "settle_error": null,
        })
    };

    assert_eq!(
        sent_line(&outcome(&[true, true])),
        Ok("Sent to 2 recipient(s) [queued for delivery]".to_string())
    );
    assert_eq!(
        sent_line(&outcome(&[true, false])),
        Ok(
            "Partial: 1/2 succeeded -- failed: r1@example.com [queued for delivery]"
                .to_string()
        )
    );
    assert_eq!(
        sent_line(&outcome(&[false, false])),
        Err("Failed to send to all 2 recipient(s)".to_string())
    );
}

/// True when the draft file `id` names contains `needle`.
fn draft_file_says(id: &str, needle: &str) -> bool {
    let path = crate::config::drafts_dir(ACCOUNT).join(format!("{id}.md"));
    std::fs::read_to_string(path)
        .map(|text| text.contains(needle))
        .unwrap_or(false)
}
