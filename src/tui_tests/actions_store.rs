//! The store-backed action flows (#0052 units A, B and C), over a real
//! ingested store.
//!
//! Three modules, and the fixture they share: the drafting flows, the
//! mutations (send, approve, mark-draft and the two batch forms) and the file
//! flows (attachments, the browser rendition, the invite source). Every one of
//! them writes its message through the ingest API and reads the result back
//! out of the store, which is why they are the root crate's tests since
//! #0126 (P5-U10e) and not `src/tui/actions.rs`'s own.
//!
//! What they assert did not change on the way here. The handlers they drive
//! are `crate::tui::actions`', reached by the paths the TUI spells them with.

use std::path::PathBuf;

use crate::draft::{create_draft_from_source, DraftFromSource, DraftRecipientEdit, SourceMessage};
use crate::selector::Selector;
use crate::tui::actions::*;
use crate::tui::app::{App, MessageRef};

use super::daemon::TestDaemon;

/// The store-backed drafting flows (#0052 unit A), over a real ingested store.
///
/// Each test writes its message through the ingest API, builds the source the
/// way the list flow does ([`crate::draft::source_from_row`]) and then asserts
/// the three things a user sees: the draft file `mp reply` / `mp forward`
/// would have written for the same source, the drafts index holding it right
/// away, and a status-line selector that resolves back to it.
#[cfg(test)]
mod store_backed_drafts {
    use super::*;
    use crate::parse::{AttachmentData, FetchedEmail};
    use crate::selector::Namespace;
    use crate::store::read::MessageRow;
    use crate::store::{BlobStore, Store};

    /// Point the data directory at a tempdir so every `config::` path resolves
    /// inside the fixture.
    ///
    /// Thread-local (#0077): no process environment is mutated, so no other
    /// test can observe this fixture's data dir and no lock is needed.
    /// Materialised message files land under `parse::test_temp_root()` for the
    /// same reason -- see the note there.
    pub(super) struct Fixture {
        _dir: crate::config::test_env::TestDataDir,
    }

    impl Fixture {
        pub(super) fn new() -> Self {
            Self {
                _dir: crate::config::test_env::TestDataDir::new(),
            }
        }

        pub(super) fn store(&self) -> Store {
            Store::open(crate::config::store_path("alice")).unwrap()
        }

        /// A [`Session`](crate::tui::session::Session) over an in-process
        /// daemon serving this fixture's data root.
        ///
        /// What an `App` needs to reach the routed helpers: since P5-U6 the
        /// attachment materialisation, the browser rendition, the draft-path
        /// lookup, the forward subject and the reply/forward builders are
        /// daemon methods, so a fixture `App` with no session gets the same
        /// answer as a wedged one, which is none. Assign it to `app.session`
        /// **after** the fixture, so the session (and its thread) is dropped
        /// before the tempdir it reads.
        pub(super) fn session(&self) -> crate::tui::session::Session {
            TestDaemon::new(&["alice"]).session()
        }

        /// Ingest one message and hand back the row the list would show.
        pub(super) fn ingest(&self, email: &FetchedEmail) -> MessageRow {
            let store = self.store();
            let blobs = BlobStore::for_account("alice");
            let outcome = crate::ingest::ingest_message(
                &store,
                &blobs,
                &crate::ingest::IngestInput {
                    account: "alice",
                    mailbox: "inbox",
                    uid: 1,
                    email,
                    raw: None,
                },
            )
            .unwrap();
            crate::store::read::find_by_id(&store, outcome.row_id)
                .unwrap()
                .unwrap()
        }

        /// The source of a reply or a forward off that row, which is what both
        /// the list flow and `mp reply` build.
        pub(super) fn source(&self, row: &MessageRow, with_attachments: bool) -> SourceMessage {
            let store = self.store();
            let blobs = BlobStore::for_account("alice");
            crate::draft::source_from_row(&store, &blobs, row, with_attachments).unwrap()
        }

        /// Resolve a selector through the drafts index, exactly as
        /// `mp send <selector>` would after reading it off the status line.
        pub(super) fn resolve(&self, selector: &Selector) -> crate::store::drafts::DraftRow {
            let store = self.store();
            let query =
                crate::selector::parse_in(&selector.to_string(), Namespace::Drafts, "alice", None)
                    .unwrap();
            crate::selector::resolve_draft(&store, &query).unwrap().0
        }
    }

    pub(super) fn fixture_email(subject: &str) -> FetchedEmail {
        FetchedEmail {
            from: "Alice <alice@example.com>".into(),
            to: "me@example.com, bob@example.com".into(),
            cc: Some("carol@example.com".into()),
            reply_to: None,
            bcc: None,
            subject: subject.into(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".into(),
            body_text: "Original body".into(),
            html_body: Some("<p>Rich body</p>".into()),
            has_attachments: false,
            message_id: Some(format!("<{subject}@example.com>")),
            attachments: Vec::new(),
            flags: crate::types::MessageFlags::seen(true),
            calendar_ics: None,
            event: None,
        }
    }

    /// Reply: the quote comes out of the body blob, the companion HTML out of
    /// the html blob, and the draft is the one `mp reply` writes for the same
    /// row. It is in the index before the status line names it.
    #[test]
    fn reply_writes_the_cli_draft_and_indexes_it_immediately() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Hello"));
        let source = fx.source(&row, false);

        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Reply { all: false },
            None,
            None,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("from: \"me@example.com\""), "{content}");
        assert!(content.contains("to: \"alice@example.com\""), "{content}");
        assert!(content.contains("subject: \"Re: Hello\""), "{content}");
        assert!(content.contains("status: draft"), "{content}");
        // A plain reply addresses the sender only.
        assert!(!content.contains("cc: \""), "{content}");
        assert!(content.contains("{{SIGNATURE}}"), "{content}");
        assert!(
            content.contains("On Mon, 01 Jan 2024 12:00:00 +0000, Alice <alice@example.com> wrote:"),
            "{content}"
        );
        assert!(content.contains("> Original body"), "{content}");

        // The sender wrote markup, so the reply quotes markup (#0050 review).
        let companion = std::fs::read_to_string(path.with_extension("html")).unwrap();
        assert!(companion.contains("<p>Rich body</p>"), "{companion}");

        // The index holds it under the selector the status line shows.
        let indexed = fx.resolve(&selector);
        assert_eq!(indexed.path, path);
        assert_eq!(indexed.status, "draft");
        assert!(
            content.contains(&format!("id: \"{}\"", indexed.id)),
            "the file carries the id it is indexed under: {content}"
        );
    }

    /// Reply-all: every other recipient of the source, To and Cc alike, minus
    /// this account's own address.
    #[test]
    fn reply_all_carries_the_other_recipients_and_not_this_account() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Meeting"));
        let source = fx.source(&row, false);

        let (path, _) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Reply { all: true },
            None,
            None,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("to: \"alice@example.com\""), "{content}");
        assert!(
            content.contains("cc: \"bob@example.com, carol@example.com\""),
            "the other recipients, deduplicated and in order: {content}"
        );
        // This account is not copied on its own reply; the only line naming it
        // is the `from:`.
        assert_eq!(
            content
                .lines()
                .filter(|l| l.contains("me@example.com"))
                .collect::<Vec<_>>(),
            vec!["from: \"me@example.com\""],
            "{content}"
        );
    }

    /// Forward: the forwarded header block plus the body, and the row's
    /// attachment blobs materialised into the stable per-account mirror that
    /// outlives the source row (#0006).
    #[test]
    fn forward_carries_the_header_block_and_the_materialised_attachments() {
        let fx = Fixture::new();
        let mut email = fixture_email("Report");
        email.has_attachments = true;
        email.attachments = vec![AttachmentData {
            filename: "report.pdf".into(),
            content: b"fake pdf".to_vec(),
            content_id: None,
        }];
        let row = fx.ingest(&email);
        let source = fx.source(&row, true);

        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Forward,
            None,
            None,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("subject: \"Fwd: Report\""), "{content}");
        assert!(content.contains("to: \"\""), "{content}");
        assert!(
            content.contains("---------- Forwarded message ----------"),
            "{content}"
        );
        assert!(content.contains("From: Alice <alice@example.com>"), "{content}");
        assert!(content.contains("Original body"), "{content}");

        let expected = crate::parse::stable_attachments_dir(
            &crate::config::account_dir("alice"),
            "<Report@example.com>",
        )
        .join("report.pdf");
        assert!(
            content.contains(expected.to_string_lossy().as_ref()),
            "the draft references the stable mirror: {content}"
        );
        assert_eq!(std::fs::read(&expected).unwrap(), b"fake pdf");

        assert_eq!(fx.resolve(&selector).path, path);
    }

    /// The forward wizard's recipients and subject win over the ones the
    /// builder derived, and the body it wrote is left alone.
    #[test]
    fn the_forward_wizard_headers_replace_the_builders() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Report"));
        let source = fx.source(&row, true);

        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Forward,
            Some(&DraftRecipientEdit {
                to: "dave@example.com".to_string(),
                cc: String::new(),
                bcc: String::new(),
                subject: "Fwd: Report (for review)".to_string(),
            }),
            None,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("to: \"dave@example.com\""), "{content}");
        assert!(
            content.contains("subject: \"Fwd: Report (for review)\""),
            "{content}"
        );
        assert!(
            content.contains("---------- Forwarded message ----------"),
            "the body survives the header rewrite: {content}"
        );

        // The index holds the edited subject, not the derived one.
        let indexed = fx.resolve(&selector);
        assert_eq!(indexed.subject.as_deref(), Some("Fwd: Report (for review)"));
    }

    /// Edit recipients resolves the draft through the index by its `id:`, not
    /// through a path the list happened to be holding, and the index is
    /// refreshed so the row matches the file the wizard just rewrote.
    #[test]
    fn edit_recipients_finds_the_draft_through_the_index() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Hello"));
        let source = fx.source(&row, false);
        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Reply { all: false },
            None,
            None,
        )
        .unwrap();

        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.session = Some(fx.session());
        let id = fx.resolve(&selector).id;
        assert_eq!(indexed_draft_path(&mut app, &id), Some(path.clone()));

        crate::draft::rewrite_draft_recipients(
            &path,
            &DraftRecipientEdit {
                to: "erin@example.com".to_string(),
                cc: String::new(),
                bcc: String::new(),
                subject: "Re: Hello, again".to_string(),
            },
        )
        .unwrap();
        crate::store::drafts::refresh_account("alice").unwrap();

        let indexed = fx.resolve(&selector);
        assert_eq!(indexed.to.as_deref(), Some("erin@example.com"));
        assert_eq!(indexed.subject.as_deref(), Some("Re: Hello, again"));

        // A draft the index no longer holds declines instead of guessing.
        assert_eq!(indexed_draft_path(&mut app, "does-not-exist"), None);
    }

    /// A server-search hit that resolved to no row still replies: the fetched
    /// content the overlay is rendering is the source, and the draft it
    /// produces is the same shape a resolved hit's would be.
    #[test]
    fn an_unresolved_search_hit_replies_from_its_fetched_content() {
        let fx = Fixture::new();
        let mut email = fixture_email("Never synced");
        email.attachments = vec![AttachmentData {
            filename: "notes.txt".into(),
            content: b"notes".to_vec(),
            content_id: None,
        }];

        let source = crate::draft::source_from_fetched(
            &crate::config::account_dir("alice"),
            &email,
            true,
        )
        .unwrap();

        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Forward,
            None,
            None,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("subject: \"Fwd: Never synced\""),
            "{content}"
        );
        assert!(content.contains("Original body"), "{content}");
        let expected = crate::parse::stable_attachments_dir(
            &crate::config::account_dir("alice"),
            "<Never synced@example.com>",
        )
        .join("notes.txt");
        assert_eq!(std::fs::read(&expected).unwrap(), b"notes");
        assert!(
            content.contains(expected.to_string_lossy().as_ref()),
            "{content}"
        );
        assert_eq!(fx.resolve(&selector).path, path);
    }
}

/// The store-backed mutation flows (#0052 unit B), over the same fixture:
/// send, approve, mark-draft and the batch forms of the last two.
///
/// The send tests run against [`crate::send::send_draft`], which is the one
/// implementation `mp send`, `mp send-approved` and this key all reach
/// (#0058), so what they pin holds for the CLI too. They are offline by
/// construction. Three halves of the contract need no server and are exactly
/// the ones worth pinning: a draft that is not approved is refused before
/// anything is enqueued, a submission that reaches nobody still leaves the
/// durable record the outbox exists for with the draft file untouched, and a
/// context naming no transport at all refuses before the draft is read.
#[cfg(test)]
mod store_backed_mutations {
    use super::store_backed_drafts::{fixture_email, Fixture};
    use super::*;
    use crate::tui::app::EmailEntry;

    /// An account that submits to a closed port: the SMTP conversation fails
    /// on connect, deterministically and without a network.
    fn dead_smtp_ctx() -> crate::send::SendContext {
        crate::send::SendContext {
            graph: None,
            smtp: Some(crate::config::SmtpConfig {
                host: "127.0.0.1".to_string(),
                port: 1,
                username: String::new(),
                password: String::new(),
                default_from: "me@example.com".to_string(),
                accept_invalid_certs: false,
                auth_method: crate::config::AuthMethod::Password,
            }),
            account: crate::config::AccountConfig {
                name: "alice".to_string(),
                default_from: "me@example.com".to_string(),
                ..Default::default()
            },
            email_settings: crate::config::EmailSettings::default(),
            signature: None,
        }
    }

    /// A received list row: a `messages` row and no draft id, which is what
    /// `entry_from_row` builds.
    fn received_entry() -> EmailEntry {
        EmailEntry {
            msg: Some(MessageRef::new(1)),
            draft_id: None,
            ..draft_entry("unused")
        }
    }

    /// A Drafts list row: the indexed id and no `messages` row, which is what
    /// `entry_from_draft` builds.
    fn draft_entry(id: &str) -> EmailEntry {
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
            status: "draft".to_string(),
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

    /// An app whose cursor sits on `id`'s Drafts row.
    fn app_on_draft(id: &str) -> App {
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.emails = std::sync::Arc::new(vec![draft_entry(id)]);
        app.rebuild_visible();
        app
    }

    /// Write one reply draft off a fresh row and hand back its file and id.
    fn a_draft(fx: &Fixture) -> (PathBuf, Selector, String) {
        let row = fx.ingest(&fixture_email("Hello"));
        let source = fx.source(&row, false);
        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Reply { all: false },
            None,
            None,
        )
        .unwrap();
        let id = fx.resolve(&selector).id;
        (path, selector, id)
    }

    fn outbox_counts(fx: &Fixture) -> crate::outbox::OutboxCounts {
        crate::outbox::counts(&fx.store(), "alice").unwrap()
    }

    /// The TUI send preamble (#0089): a draft that fails validation keeps its
    /// `draft` status. Approval must never persist off a send that was
    /// refused, or a later send would skip the approve-and-send warning the
    /// confirm dialog shows for an unapproved draft.
    #[test]
    fn a_draft_that_fails_validation_is_not_marked_approved() {
        let fx = Fixture::new();
        let (path, _selector, _id) = a_draft(&fx);
        // Blank the recipients: validate_draft refuses a draft whose to, cc
        // and bcc are all empty.
        let text = std::fs::read_to_string(&path).unwrap();
        let unaddressed = text.replacen("\nto:", "\nto: \"\"\nx-to:", 1);
        std::fs::write(&path, &unaddressed).unwrap();

        let err = validate_then_approve(&path).unwrap_err();
        assert!(format!("{err:#}").contains("No recipients"), "{err:#}");
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("status: draft"),
            "a refused send must not persist an approved flag"
        );

        // With the recipients restored the same preamble approves the draft.
        std::fs::write(&path, &text).unwrap();
        validate_then_approve(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("status: approved"));
    }

    /// Regression: `validate_then_approve` must hand back the *approved*
    /// draft, not the copy it parsed before the approval write.
    ///
    /// `mark_as_approved` rewrites `status:` in the file only, so returning
    /// the pre-approval struct gave [`Action::Send`] a value still reading
    /// `status: draft`; `build_draft_message` reads that in-memory status and
    /// refused every `x` approve-and-send with "Email not approved for
    /// sending. Current status: draft", even though the file on disk was
    /// approved. The two assertions are the two halves of that bug: the
    /// returned status, and the send actually reaching the outbox.
    #[test]
    fn approve_and_send_returns_the_approved_draft_and_reaches_the_outbox() {
        let fx = Fixture::new();
        let (path, _selector, _id) = a_draft(&fx);

        let draft = validate_then_approve(&path).unwrap();
        assert_eq!(
            draft.frontmatter.status,
            crate::types::EmailStatus::Approved,
            "the returned draft must carry the approval that was just written"
        );

        // The transport is dead, so the send fails at submission -- but it
        // fails *after* the outbox commit, which is only reachable once
        // `build_draft_message` accepts the status. A refusal would leave the
        // outbox empty (see the unapproved-draft test above).
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sent = rt
            .block_on(crate::send::send_draft(&draft, &dead_smtp_ctx()))
            .unwrap();
        assert!(
            sent.report.row_id.is_some(),
            "an approved draft must reach the outbox instead of being refused"
        );
        assert_eq!(outbox_counts(&fx).total(), 1);
    }

    /// The approved-status requirement is `mp send`'s, and it lives in
    /// [`crate::send::build_draft_message`], which runs before the outbox row
    /// is written: a draft that is not approved is refused with the CLI's own
    /// message and leaves nothing behind.
    #[test]
    fn send_refuses_an_unapproved_draft_before_it_reaches_the_outbox() {
        let fx = Fixture::new();
        let (path, _selector, _id) = a_draft(&fx);
        let draft = crate::draft::parse_email_draft(&path).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = match rt.block_on(crate::send::send_draft(&draft, &dead_smtp_ctx())) {
            Ok(_) => panic!("an unapproved draft must not be sent"),
            Err(e) => e,
        };

        let text = format!("{err:#}");
        assert!(text.contains("Email not approved for sending"), "{text}");
        assert!(text.contains("Current status: draft"), "{text}");
        assert_eq!(outbox_counts(&fx).total(), 0, "nothing was enqueued");
        assert!(std::fs::read_to_string(&path).unwrap().contains("status: draft"));
    }

    /// An approved draft is committed to the outbox before the submission is
    /// attempted, so a send that reaches nobody leaves a durable `failed` row
    /// and a draft that is still approved rather than a message lost between
    /// the two.
    #[test]
    fn a_send_that_reaches_nobody_leaves_a_failed_outbox_row_and_an_unsent_draft() {
        let fx = Fixture::new();
        let (path, selector, _id) = a_draft(&fx);
        crate::draft::mark_as_approved(&path).unwrap();
        crate::store::drafts::refresh_account("alice").unwrap();
        let draft = crate::draft::parse_email_draft(&path).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let sent = rt
            .block_on(crate::send::send_draft(&draft, &dead_smtp_ctx()))
            .unwrap();
        let report = sent.report;

        assert!(!report.send_result.any_succeeded());
        assert!(
            sent.settle_error.is_none(),
            "nothing was sent, so nothing was retired"
        );
        assert!(report.row_id.is_some(), "the message reached the outbox");
        assert!(matches!(
            report.state,
            Some(crate::outbox::OutboxState::Failed)
        ));
        assert_eq!(outbox_counts(&fx).failed, 1);

        // Nobody took it, which is what the status line the daemon's outcome
        // is rendered into says (`commands::sent_line`); and the draft is
        // untouched, so nothing was marked sent.
        assert!(
            report.send_result.failed().len() == report.send_result.results.len(),
            "every recipient was refused"
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("status: approved"));
        assert_eq!(fx.resolve(&selector).status, "approved");
    }

    /// A context that names neither transport is a configuration error, and it
    /// is caught before the outbox hears about the draft: the same refusal the
    /// TUI shows when `resolve_send_account` finds no SMTP and no Graph.
    #[test]
    fn a_context_with_no_transport_refuses_before_anything_is_enqueued() {
        let fx = Fixture::new();
        let (path, _selector, _id) = a_draft(&fx);
        crate::draft::mark_as_approved(&path).unwrap();
        let draft = crate::draft::parse_email_draft(&path).unwrap();
        let ctx = crate::send::SendContext {
            smtp: None,
            ..dead_smtp_ctx()
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = match rt.block_on(crate::send::send_draft(&draft, &ctx)) {
            Ok(_) => panic!("a draft with no transport must not be sent"),
            Err(e) => e,
        };

        assert_eq!(err.to_string(), "SMTP not configured");
        assert_eq!(outbox_counts(&fx).total(), 0, "nothing was enqueued");
    }

    /// `$EDITOR` opens the file the index holds for the row under the cursor.
    ///
    /// A received row never reaches this resolver: it is materialised out of
    /// the store instead (#0075, covered in `store_backed_files`), and the
    /// decline below is what is left for an entry that is neither.
    #[test]
    fn edit_current_resolves_the_cursor_draft_through_the_index() {
        let fx = Fixture::new();
        let (path, _selector, id) = a_draft(&fx);
        let mut app = app_on_draft(&id);
        // `draft.path` since P5-U6, which answers from a fresh scan of the
        // drafts directory the way the store-backed lookup did.
        app.session = Some(fx.session());

        assert_eq!(
            cursor_draft(&mut app, "never shown"),
            Some((id, path)),
            "the cursor's draft resolves to the file the index holds"
        );

        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.emails = std::sync::Arc::new(vec![received_entry()]);
        app.rebuild_visible();
        assert_eq!(
            cursor_draft(&mut app, "Open in $EDITOR needs a message or a draft"),
            None
        );
        let status = app.status_message.clone().unwrap();
        assert_eq!(status, "Open in $EDITOR needs a message or a draft");
        assert!(!status.contains("#0052"), "{status}");
    }
}

/// The store-backed file flows (#0052 unit C), over the same fixture:
/// attachments, the browser rendition and the invite source.
///
/// Every one of them used to read a file the ingest wrote beside a `.md`.
/// What is pinned here is that the bytes now come out of `message_blobs` and
/// land where the CLI puts them, that the naming a save collision produces is
/// the pre-nuke one, and that a server-search hit with no local row is served
/// from the fetch rather than declined.
#[cfg(test)]
mod store_backed_files {
    use super::store_backed_drafts::{fixture_email, Fixture};
    use super::*;
    use crate::parse::{AttachmentData, FetchedEmail};
    use crate::store::BlobStore;
    use crate::store::read::MessageRow;
    use crate::tui::app::EmailEntry;

    fn attachment(name: &str, bytes: &[u8]) -> AttachmentData {
        AttachmentData {
            filename: name.to_string(),
            content: bytes.to_vec(),
            content_id: None,
        }
    }

    /// An app whose cursor sits on the list row for `row`, with a session
    /// onto an in-process daemon over `fx`'s data root.
    ///
    /// The session is what makes the file flows work at all since P5-U6: the
    /// attachment materialisation and the browser rendition are
    /// `message.materialise_attachment` and `message.materialise_html` now,
    /// and an `App` with no session reaches neither.
    fn app_on_row(fx: &Fixture, row: &MessageRow) -> App {
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.session = Some(fx.session());
        app.emails = std::sync::Arc::new(vec![EmailEntry {
            msg: Some(MessageRef::new(row.id)),
            draft_id: None,
            skip: None,
            selector: None,
            from: row.from.clone().unwrap_or_default(),
            to: row.to.clone().unwrap_or_default(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: row.subject.clone().unwrap_or_default(),
            status: "inbox".to_string(),
            date_display: row.date_display.clone().unwrap_or_default(),
            date_sort: String::new(),
            has_attachments: true,
            read: true,
            answered: false,
            forwarded: false,
            flagged: false,
            is_invite: false,
        }]);
        app.rebuild_visible();
        app
    }

    /// `o` and `O` on a received row resolve the row's blobs into files, one
    /// per attachment, in the row's own order and with the sender's names.
    ///
    /// The directory moved in P5-U6: `message.materialise_attachment` writes
    /// each part into a handle directory of the daemon's own
    /// (`<data>/runtime/handles/<handle>/<name>`, one directory per handle so
    /// two senders' `report.pdf` cannot collide), where the store-backed
    /// helper wrote them all into `parse::materialisation_dir(<row id>)`
    /// beside `mp open`'s. Both are 0700 and private; what the picker and the
    /// save pipeline address is a file either way. The one behavioural
    /// consequence is lifetime: a handle expires (ten minutes by default)
    /// where a materialised copy lived until the temp directory was swept.
    #[test]
    fn the_cursor_row_materialises_its_blobs_into_daemon_handles() {
        let fx = Fixture::new();
        let mut email = fixture_email("With files");
        email.has_attachments = true;
        email.attachments = vec![
            attachment("notes.txt", b"notes"),
            attachment("report.pdf", b"%PDF-1.4"),
        ];
        let row = fx.ingest(&email);
        let mut app = app_on_row(&fx, &row);

        let files = cursor_attachment_files(&mut app).unwrap();

        assert_eq!(files.len(), 2, "{files:?}");
        let handles = crate::daemon::runtime::runtime_dir().join("handles");
        for file in &files {
            assert_eq!(
                file.parent().and_then(|dir| dir.parent()),
                Some(handles.as_path()),
                "{file:?} is not under the daemon's handle directory"
            );
        }
        assert_ne!(
            files[0].parent(),
            files[1].parent(),
            "one directory per handle, so two names cannot collide"
        );
        // And it is private to this user (0700), because `$TMPDIR` is not.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = files[0].parent().unwrap();
            let mode = std::fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        }
        let by_name: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(by_name, vec!["notes.txt", "report.pdf"]);
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"notes");
        assert_eq!(std::fs::read(&files[1]).unwrap(), b"%PDF-1.4");
    }

    /// A message with no attachments is not an error: the empty list is what
    /// the picker turns into "No attachments".
    #[test]
    fn a_row_without_attachments_resolves_to_an_empty_list() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Bare"));
        let mut app = app_on_row(&fx, &row);

        assert_eq!(cursor_attachment_files(&mut app), Some(Vec::new()));
        assert!(app.status_message.is_none(), "{:?}", app.status_message);
    }

    /// A row that is neither a message nor an indexed draft (a parse-skipped
    /// draft file, a server-search hit that resolved to nothing) has no
    /// attachments to reach, and the status line says so.
    #[test]
    fn a_row_with_no_identity_declines_the_attachment_key() {
        let _fx = Fixture::new();
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.emails = std::sync::Arc::new(vec![draft_entry(None)]);
        app.rebuild_visible();

        assert_eq!(cursor_attachment_files(&mut app), None);
        assert_eq!(
            app.status_message.clone().unwrap(),
            "Attachments needs a message or a readable draft; this row has neither"
        );
    }

    /// A list entry for a draft, or (with `None`) for a row that carries no
    /// identity at all.
    fn draft_entry(draft_id: Option<&str>) -> EmailEntry {
        EmailEntry {
            msg: None,
            draft_id: draft_id.map(str::to_string),
            skip: None,
            selector: None,
            from: String::new(),
            to: "alice@example.com".to_string(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "Re: Hello".to_string(),
            status: "draft".to_string(),
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

    /// Write a draft naming `attachments` and index it, returning the app with
    /// the cursor on it.
    fn app_on_draft(fx: &Fixture, attachments: &[String]) -> App {
        let dir = crate::config::drafts_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        let listed: String = attachments.iter().map(|a| format!("  - \"{a}\"\n")).collect();
        let body = if listed.is_empty() {
            "attachments:\n".to_string()
        } else {
            format!("attachments:\n{listed}")
        };
        std::fs::write(
            dir.join("note.md"),
            format!("---\nto: bob@example.com\nsubject: Files\nstatus: draft\n{body}---\n\nBody\n"),
        )
        .unwrap();
        let store = fx.store();
        let rows = crate::store::drafts::refresh(&store, "alice", &dir).unwrap();

        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        // The draft branch resolves its file through `draft.path` since P5-U6,
        // even though the attachments themselves are the paths its own
        // frontmatter names (#0016) and reach no method.
        app.session = Some(fx.session());
        app.emails = std::sync::Arc::new(vec![draft_entry(Some(&rows[0].id))]);
        app.rebuild_visible();
        app
    }

    /// `o` on a draft opens the files the draft names (#0016): the real paths,
    /// not a temp copy, because those are the bytes that will be sent.
    #[test]
    fn a_draft_answers_the_attachment_key_from_its_own_frontmatter() {
        let fx = Fixture::new();
        let files_dir = crate::config::account_dir("alice").join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        let one = files_dir.join("report.pdf");
        let two = files_dir.join("notes.txt");
        std::fs::write(&one, b"%PDF-1.4").unwrap();
        std::fs::write(&two, b"notes").unwrap();

        let listed = vec![one.display().to_string(), two.display().to_string()];
        let mut app = app_on_draft(&fx, &listed);

        assert_eq!(cursor_attachment_files(&mut app), Some(vec![one, two]));
        assert!(app.status_message.is_none(), "{:?}", app.status_message);
    }

    /// A draft with no `attachments:` is not an error: the empty list is what
    /// the picker turns into "No attachments", the same as a bare message.
    #[test]
    fn a_draft_without_attachments_resolves_to_an_empty_list() {
        let fx = Fixture::new();
        let mut app = app_on_draft(&fx, &[]);

        assert_eq!(cursor_attachment_files(&mut app), Some(Vec::new()));
        assert!(app.status_message.is_none(), "{:?}", app.status_message);
    }

    /// A path that is no longer there is named, not skipped: a stale entry is
    /// exactly what `o` is pressed to find out about before `mp send` hits it.
    #[test]
    fn a_missing_draft_attachment_is_named_on_the_status_line() {
        let fx = Fixture::new();
        let files_dir = crate::config::account_dir("alice").join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        let present = files_dir.join("here.txt");
        std::fs::write(&present, b"here").unwrap();
        let gone = files_dir.join("gone.txt").display().to_string();

        let mut app = app_on_draft(&fx, &[present.display().to_string(), gone.clone()]);
        assert_eq!(cursor_attachment_files(&mut app), Some(vec![present]));
        let status = app.status_message.clone().unwrap();
        assert!(status.starts_with("1 attachment missing: "), "{status}");
        assert!(status.contains(&gone), "{status}");

        // Every path gone is a failure, not an empty picker saying "none".
        let mut app = app_on_draft(&fx, std::slice::from_ref(&gone));
        assert_eq!(cursor_attachment_files(&mut app), None);
        assert!(app.status_message.clone().unwrap().contains(&gone));
    }

    /// Save writes the materialised file into the chosen directory, and a
    /// second save of the same name does not overwrite the first: the
    /// `_1` suffix is the pre-nuke collision rule, unchanged because the save
    /// half still copies files.
    #[test]
    fn saving_the_same_attachment_twice_keeps_both_copies() {
        let fx = Fixture::new();
        let mut email = fixture_email("Twice");
        email.has_attachments = true;
        email.attachments = vec![attachment("notes.txt", b"notes")];
        let row = fx.ingest(&email);
        let mut app = app_on_row(&fx, &row);
        let files = cursor_attachment_files(&mut app).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let first = crate::parse::save_attachment(&files[0], dest.path()).unwrap();
        let second = crate::parse::save_attachment(&files[0], dest.path()).unwrap();

        assert_eq!(first, dest.path().join("notes.txt"));
        assert_eq!(second, dest.path().join("notes_1.txt"));
        assert_eq!(std::fs::read(&first).unwrap(), b"notes");
        assert_eq!(std::fs::read(&second).unwrap(), b"notes");
    }

    /// `b` writes the html blob to a file and hands the browser that: the
    /// markup is the sender's own, not a re-render of the plain text.
    #[test]
    fn the_browser_gets_the_html_blob_written_to_a_file() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Rich"));
        let mut app = app_on_row(&fx, &row);

        let path = html_rendition_for_row(&mut app, row.id).unwrap();

        assert_eq!(path.extension().unwrap(), "html");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.ends_with("<p>Rich body</p>"), "{written}");
        // The file is served with no HTTP headers, so it carries its own
        // charset and CSP.
        assert!(written.starts_with("<meta http-equiv=\"Content-Security-Policy\""), "{written}");
        assert!(written.contains("<meta charset=\"UTF-8\">"), "{written}");
    }

    /// A message whose HTML points at a `cid:` image part: the browser file
    /// carries the image as a `data:` URI, because a browser opening a file
    /// from disk has no message to resolve `cid:` URLs against.
    #[test]
    fn the_browser_rendition_inlines_cid_images_as_data_uris() {
        let fx = Fixture::new();
        let html = "<p><img src=\"cid:logo@x\"></p>";
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let raw = format!(
            "From: a@example.com\r\nSubject: hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"B\"\r\n\r\n\
--B\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n\
--B\r\nContent-Type: image/png; name=\"logo.png\"\r\nContent-ID: <logo@x>\r\n\
Content-Transfer-Encoding: base64\r\nContent-Disposition: inline; filename=\"logo.png\"\r\n\r\n{png_b64}\r\n--B--\r\n"
        );
        let mut email = fixture_email("Logo");
        email.html_body = Some(html.to_string());
        email.has_attachments = true;
        let store = fx.store();
        let blobs = BlobStore::for_account("alice");
        let outcome = crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: "alice",
                mailbox: "inbox",
                uid: 1,
                email: &email,
                raw: Some(raw.as_bytes()),
            },
        )
        .unwrap();
        let row = crate::store::read::find_by_id(&store, outcome.row_id)
            .unwrap()
            .unwrap();
        let mut app = app_on_row(&fx, &row);

        let path = html_rendition_for_row(&mut app, row.id).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.to_ascii_lowercase().contains("cid:"), "{written}");
        assert!(written.contains("data:image/png;base64,"), "{written}");
    }

    /// A sender who wrote no markup has no rendition, which is a status line
    /// rather than an error or an empty page.
    #[test]
    fn a_message_without_html_says_so_instead_of_opening_an_empty_page() {
        let fx = Fixture::new();
        let mut email = fixture_email("Plain");
        email.html_body = None;
        let row = fx.ingest(&email);
        let mut app = app_on_row(&fx, &row);

        assert_eq!(html_rendition_for_row(&mut app, row.id), None);
        assert_eq!(
            app.status_message.as_deref(),
            Some("No HTML version available")
        );
    }

    /// The server-search hit that resolved to no local row: its attachments
    /// and its markup are the bytes the overlay is already holding, written
    /// out so the picker and the browser see files either way.
    #[test]
    fn an_unresolved_search_hit_is_served_from_the_fetch() {
        let _fx = Fixture::new();
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        let fetched = FetchedEmail {
            attachments: vec![attachment("../escape.txt", b"payload")],
            has_attachments: true,
            ..fixture_email("Never synced")
        };

        let files = fetched_attachment_files(&mut app, &fetched, 7).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].file_name().unwrap().to_string_lossy(),
            ".._escape.txt",
            "the filename is sanitised, so a hostile one cannot escape the temp dir"
        );
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"payload");

        let html = fetched.html_body.clone().unwrap();
        let page = html_rendition(&mut app, &html, "search-7").unwrap();
        let page_html = std::fs::read_to_string(&page).unwrap();
        assert!(page_html.ends_with("<p>Rich body</p>"), "{page_html}");
        assert!(page_html.contains("Content-Security-Policy"), "{page_html}");
    }

    /// `e` on a received row writes the store's own rendition of the message
    /// where the browser rendition and every materialised attachment go, names
    /// it after the subject so the editor's buffer title is recognisable, and
    /// makes it unwritable (#0075).
    ///
    /// The directory moved in P5-U10c: `message.materialise_markdown` writes
    /// it into a handle directory of the daemon's own runtime, where the
    /// store-backed helper wrote it under `parse::materialisation_dir(<row>)`.
    /// Both are private and what `$EDITOR` is handed is a file either way; the
    /// new one is released rather than unlinked, which is what the release row
    /// below asserts.
    #[test]
    fn the_read_only_view_lands_beside_the_other_renditions() {
        let fx = Fixture::new();
        let mut email = fixture_email("Quarterly Report: Q3");
        email.has_attachments = true;
        email.attachments = vec![attachment("report.pdf", b"%PDF-1.4")];
        let row = fx.ingest(&email);
        let mut app = app_on_row(&fx, &row);

        let rendition = readonly_view_for_row(&mut app, row.id).unwrap();
        let path = rendition.path.clone();

        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "quarterly-report-q3.md",
            "the subject names the buffer"
        );
        assert!(
            path.starts_with(crate::config::mailypoppins_data_dir().join("runtime/handles")),
            "a rendition lands in the handle family's own directory, got {}",
            path.display()
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("---\n"), "{content}");
        assert!(content.contains("subject: 'Quarterly Report: Q3'\n"), "{content}");
        assert!(content.contains("mailbox: inbox\n"), "{content}");
        assert!(content.contains("read: true\n"), "{content}");
        assert!(content.contains("answered: false\n"), "{content}");
        assert!(content.contains("forwarded: false\n"), "{content}");
        assert!(content.contains("- report.pdf\n"), "{content}");
        assert!(content.ends_with("Original body\n"), "{content}");

        // 0444: the editor opens the buffer read-only and says so, instead of
        // letting anyone believe a save reaches the message.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o444, "{mode:o}");
        }

        // A second open is a second handle over the same row, in a directory
        // of its own: the mode the first one left cannot stop it, which is
        // what the pre-daemon `write_readonly` had to remove the file for.
        let again = readonly_view_for_row(&mut app, row.id).unwrap();
        assert_ne!(again.handle, rendition.handle);
        assert_ne!(again.path, path);
        assert_eq!(
            std::fs::read_to_string(&again.path).unwrap(),
            content,
            "the rendition is rebuilt from the store on every open"
        );
    }

    /// The rendition is scratch whichever way the editor session ends: a
    /// clean exit and an editor that failed both come back with no 0444 file
    /// left on disk (#0075).
    #[test]
    fn the_read_only_view_is_discarded_however_the_editor_exits() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Quarterly Report"));
        let mut app = app_on_row(&fx, &row);

        let rendition = readonly_view_for_row(&mut app, row.id).unwrap();
        assert!(rendition.path.exists());
        finish_readonly_view(&mut app, &rendition, Ok(()));
        assert!(
            !rendition.path.exists(),
            "a clean exit releases the handle, which unlinks the rendition"
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Returned from the read-only copy (edits do not reach the message)")
        );

        // An editor that never launched, or exited non-zero, leaves nothing
        // behind either -- the status line is the only difference.
        let rendition = readonly_view_for_row(&mut app, row.id).unwrap();
        assert!(rendition.path.exists());
        finish_readonly_view(
            &mut app,
            &rendition,
            Err(anyhow::anyhow!("editor exited with 1")),
        );
        assert!(
            !rendition.path.exists(),
            "a failed editor takes the rendition with it too"
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Open failed: editor exited with 1")
        );
    }

    /// A message with no subject still gets a name a human can read, and one
    /// with no store row is a status line rather than an empty buffer.
    #[test]
    fn the_read_only_view_names_a_subjectless_message_by_its_row() {
        let fx = Fixture::new();
        let mut email = fixture_email("placeholder");
        email.subject = String::new();
        let row = fx.ingest(&email);
        let mut app = app_on_row(&fx, &row);

        let rendition = readonly_view_for_row(&mut app, row.id).unwrap();
        assert_eq!(
            rendition.path.file_name().unwrap().to_string_lossy(),
            format!("message-{}.md", row.id)
        );

        assert_eq!(readonly_view_for_row(&mut app, row.id + 999), None);
        assert_eq!(
            app.status_message.as_deref(),
            Some("Open failed: that message is no longer in the store")
        );
    }

    /// The agenda's Open-source reads the invite's own ics blob off the row
    /// the `CalendarEvent` carries, which is what the action writes to the
    /// file `$EDITOR` is handed.
    #[test]
    fn the_event_source_resolves_the_invites_ics_blob() {
        let fx = Fixture::new();
        let ics = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
        let mut email = fixture_email("Standup");
        email.calendar_ics = Some(ics.to_vec());
        let row = fx.ingest(&email);

        let store = fx.store();
        let blobs = BlobStore::for_account("alice");
        let source = crate::store::read::load_invite_ics(&store, &blobs, row.id).unwrap();

        assert_eq!(source, ics.to_vec());
        // A message that carries no invite has no source to open.
        let plain = fx.ingest(&fixture_email("Receipt"));
        assert_eq!(
            crate::store::read::load_invite_ics(&store, &blobs, plain.id),
            None
        );
    }
}
