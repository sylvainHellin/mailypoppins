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

        /// Ingest one message from its raw RFC822 bytes, kept as the `raw`
        /// blob the way the IMAP path keeps them.
        pub(super) fn ingest_raw(&self, raw: &[u8]) -> MessageRow {
            let email = crate::parse::parse_rfc822_to_fetched_email(raw).unwrap();
            let store = self.store();
            let blobs = BlobStore::for_account("alice");
            let outcome = crate::ingest::ingest_message(
                &store,
                &blobs,
                &crate::ingest::IngestInput {
                    account: "alice",
                    mailbox: "inbox",
                    uid: 1,
                    email: &email,
                    raw: Some(raw),
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
            server_has_attachments: None,
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
        assert!(content.contains(crate::draft::QUOTE_MARKER), "{content}");
        assert!(
            !content.contains(crate::draft::LEGACY_QUOTE_MARKER),
            "{content}"
        );
        assert!(
            content
                .contains("On Mon, 01 Jan 2024 12:00:00 +0000, Alice <alice@example.com> wrote:"),
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

    /// Reply and reply-all to a message this account sent (PERSO-99): the
    /// draft goes back to its recipients, not to this account.
    #[test]
    fn reply_to_a_sent_message_goes_to_its_recipients() {
        let fx = Fixture::new();
        let mut email = fixture_email("Sent plan");
        email.from = "Me <me@example.com>".into();
        email.to = "bob@example.com, me@example.com".into();
        let row = fx.ingest(&email);
        let source = fx.source(&row, false);

        for (all, cc) in [(false, None), (true, Some("carol@example.com"))] {
            let (path, _) = create_draft_from_source(
                "alice",
                "me@example.com",
                &source,
                DraftFromSource::Reply { all },
                None,
                None,
            )
            .unwrap();
            let draft = crate::draft::parse_email_draft(&path).unwrap();
            assert_eq!(draft.frontmatter.to.as_deref(), Some("bob@example.com"));
            assert_eq!(draft.frontmatter.cc.as_deref(), cc, "reply-all: {all}");
        }
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
            DraftFromSource::Forward {
                as_attachment: false,
            },
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
        assert!(
            content.contains("From: Alice <alice@example.com>"),
            "{content}"
        );
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

    /// An Outlook-shaped message: a multipart/related HTML body whose
    /// signature logo is a `cid:`-referenced image part, inside a
    /// multipart/mixed carrying a real PDF and an attached photo the HTML
    /// does not point at.
    pub(super) fn signature_logo_message() -> String {
        "From: Alice <alice@example.com>\r\nTo: me@example.com\r\n\
Subject: Signed\r\nMessage-ID: <signed@example.com>\r\n\
Date: Mon, 01 Jan 2024 12:00:00 +0000\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"MIX\"\r\n\r\n\
--MIX\r\nContent-Type: multipart/related; boundary=\"REL\"\r\n\r\n\
--REL\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
<p>See attached.</p><img src=\"cid:image001.png@01DA0000.00000000\">\r\n\
--REL\r\nContent-Type: image/png; name=\"image001.png\"\r\n\
Content-ID: <image001.png@01DA0000.00000000>\r\n\
Content-Disposition: inline; filename=\"image001.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nbG9nby1ieXRlcw==\r\n\
--REL--\r\n\
--MIX\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nZmFrZSBwZGY=\r\n\
--MIX\r\nContent-Type: image/png; name=\"photo.png\"\r\n\
Content-Disposition: attachment; filename=\"photo.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\ncGhvdG8tYnl0ZXM=\r\n\
--MIX--\r\n"
            .to_string()
    }

    fn attachment_names(source: &SourceMessage) -> Vec<String> {
        source
            .attachments
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// A forward lists only the real attachments: the signature logo the HTML
    /// references by `cid:` stays behind, the PDF and the unreferenced photo
    /// go along.
    #[test]
    fn forward_drops_the_cid_referenced_signature_logo() {
        let fx = Fixture::new();
        let row = fx.ingest_raw(signature_logo_message().as_bytes());
        let store = fx.store();
        let listed: Vec<(String, bool)> = crate::store::read::attachments_for(&store, row.id)
            .unwrap()
            .into_iter()
            .map(|a| (a.name, a.inline))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("image001.png".to_string(), true),
                ("report.pdf".to_string(), false),
                ("photo.png".to_string(), false),
            ],
            "ingest marks the logo inline and the listing keeps it, flagged (PERSO-165)"
        );

        let source = fx.source(&row, true);
        assert_eq!(
            attachment_names(&source),
            vec!["report.pdf".to_string(), "photo.png".to_string()]
        );
        assert_eq!(std::fs::read(&source.attachments[0]).unwrap(), b"fake pdf");
        assert_eq!(
            std::fs::read(&source.attachments[1]).unwrap(),
            b"photo-bytes"
        );

        let (path, _) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Forward {
                as_attachment: false,
            },
            None,
            None,
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("report.pdf"), "{content}");
        assert!(content.contains("photo.png"), "{content}");
        assert!(!content.contains("image001.png"), "{content}");
    }

    /// A row with no raw message (a Graph row) forwards only its real
    /// attachments too: ingest marked the logo inline from its `Content-ID`
    /// and the HTML, which is what the forward reads (PERSO-165).
    #[test]
    fn forward_without_a_raw_message_drops_the_signature_logo_too() {
        let fx = Fixture::new();
        let raw = signature_logo_message();
        let email = crate::parse::parse_rfc822_to_fetched_email(raw.as_bytes()).unwrap();
        assert!(email
            .html_body
            .as_deref()
            .is_some_and(|h| h.contains("cid:image001.png")));
        let row = fx.ingest(&email);

        let source = fx.source(&row, true);
        let mut names = attachment_names(&source);
        names.sort();
        assert_eq!(names, vec!["photo.png", "report.pdf"]);
    }

    /// A row with a raw message an older build stored carries no inline mark,
    /// and the forward re-parses the raw to drop the logo, as `b4209c44` did.
    #[test]
    fn forward_of_an_unmarked_row_re_parses_its_raw_message() {
        let fx = Fixture::new();
        let row = fx.ingest_raw(signature_logo_message().as_bytes());
        fx.store()
            .conn()
            .execute(
                "UPDATE message_blobs SET content_id = NULL, is_inline = NULL
                 WHERE message_row = ?1",
                [row.id],
            )
            .unwrap();
        let listed = crate::store::read::attachments_for(&fx.store(), row.id).unwrap();
        assert_eq!(listed.len(), 3, "an unmarked row lists every part");

        let source = fx.source(&row, true);
        assert_eq!(
            attachment_names(&source),
            vec!["report.pdf".to_string(), "photo.png".to_string()]
        );
    }

    /// A Graph row an older build stored carries no inline mark and no raw
    /// message to decide by, so until the backfill reaches it the forward
    /// keeps every attachment rather than guess.
    #[test]
    fn forward_of_an_unmarked_row_without_a_raw_message_keeps_every_attachment() {
        let fx = Fixture::new();
        let raw = signature_logo_message();
        let email = crate::parse::parse_rfc822_to_fetched_email(raw.as_bytes()).unwrap();
        let row = fx.ingest(&email);
        fx.store()
            .conn()
            .execute(
                "UPDATE message_blobs SET content_id = NULL, is_inline = NULL
                 WHERE message_row = ?1",
                [row.id],
            )
            .unwrap();

        let source = fx.source(&row, true);
        let mut names = attachment_names(&source);
        names.sort();
        assert_eq!(names, vec!["image001.png", "photo.png", "report.pdf"]);
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
            DraftFromSource::Forward {
                as_attachment: false,
            },
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

    /// Edit recipients (`ce`) is the daemon's `draft.set_headers`: the TUI
    /// names the draft by its `id:` and writes nothing itself, the daemon
    /// rewrites the four fields and leaves the body alone, and the status line
    /// names the draft.
    #[test]
    fn edit_recipients_rewrites_the_headers_through_the_daemon() {
        use crate::tui::app::{ComposeField, ComposeMode, ComposeWizard};
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
        let body_before = crate::draft::parse_email_draft(&path)
            .unwrap()
            .body_markdown;

        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.session = Some(fx.session());
        let id = fx.resolve(&selector).id;
        let wizard = ComposeWizard {
            mode: ComposeMode::EditDraft { id: id.clone() },
            to: "erin@example.com".to_string(),
            cc: String::new(),
            bcc: "boss@example.com".to_string(),
            subject: "Re: Hello, again".to_string(),
            body: String::new(),
            focus: ComposeField::To,
            signature_name: None,
            signature_initial: None,
            signature_edited: false,
            available_signatures: Vec::new(),
            suggestions: Vec::new(),
            suggestion_idx: 0,
            contacts: None,
        };
        let edit = DraftRecipientEdit {
            to: wizard.to.clone(),
            cc: wizard.cc.clone(),
            bcc: wizard.bcc.clone(),
            subject: wizard.subject.clone(),
        };
        submit_header_edit(&mut app, &wizard, &id, &edit);

        let draft = crate::draft::parse_email_draft(&path).unwrap();
        assert_eq!(draft.frontmatter.to.as_deref(), Some("erin@example.com"));
        assert_eq!(draft.frontmatter.bcc.as_deref(), Some("boss@example.com"));
        assert_eq!(draft.frontmatter.subject, "Re: Hello, again");
        assert_eq!(draft.body_markdown, body_before, "the body is untouched");
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|s| s.starts_with("Recipients updated: mp://alice/drafts/")),
            "{:?}",
            app.status_message
        );

        // A draft the daemon does not hold is refused in its words.
        submit_header_edit(&mut app, &wizard, "does-not-exist", &edit);
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|s| s.starts_with("Recipient update failed:")),
            "{:?}",
            app.status_message
        );
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

        let source =
            crate::draft::source_from_fetched(&crate::config::account_dir("alice"), &email, true)
                .unwrap();

        let (path, selector) = create_draft_from_source(
            "alice",
            "me@example.com",
            &source,
            DraftFromSource::Forward {
                as_attachment: false,
            },
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
            draft_sync: None,
            msg: Some(MessageRef::new(1)),
            draft_id: None,
            ..draft_entry("unused")
        }
    }

    /// A Drafts list row: the indexed id and no `messages` row, which is what
    /// `entry_from_draft` builds.
    fn draft_entry(id: &str) -> EmailEntry {
        EmailEntry {
            draft_sync: None,
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
            conv: None,
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
    /// confirm dialog shows for an unapproved draft. Both steps are the
    /// daemon's `draft.validate` and `draft.approve`.
    #[test]
    fn a_draft_that_fails_validation_is_not_marked_approved() {
        let fx = Fixture::new();
        let (path, _selector, id) = a_draft(&fx);
        let session = fx.session();
        // Blank the recipients: validate_draft refuses a draft whose to, cc
        // and bcc are all empty.
        let text = std::fs::read_to_string(&path).unwrap();
        let unaddressed = text.replacen("\nto:", "\nto: \"\"\nx-to:", 1);
        std::fs::write(&path, &unaddressed).unwrap();

        let err = validate_then_approve(&session, "alice", &id).unwrap_err();
        assert!(err.contains("No recipients"), "{err}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("status: draft"),
            "a refused send must not persist an approved flag"
        );

        // With the recipients restored the same preamble approves the draft.
        std::fs::write(&path, &text).unwrap();
        validate_then_approve(&session, "alice", &id).unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("status: approved"));
    }

    /// The draft `validate_then_approve` approved is one the send path
    /// accepts: the status is in the file the daemon rewrote, so a parse of it
    /// reads `approved` and the send reaches the outbox.
    ///
    /// The regression this row was written for (the approve returned the copy
    /// parsed before the approval write, still reading `status: draft`, and
    /// `build_draft_message` refused every `x`) cannot recur now that nothing
    /// is returned, and the outbox half still pins the end of the chain.
    #[test]
    fn approve_and_send_returns_the_approved_draft_and_reaches_the_outbox() {
        let fx = Fixture::new();
        let (path, _selector, id) = a_draft(&fx);

        validate_then_approve(&fx.session(), "alice", &id).unwrap();
        let draft = crate::draft::parse_email_draft(&path).unwrap();
        assert_eq!(
            draft.frontmatter.status,
            crate::types::EmailStatus::Approved,
            "the draft on disk carries the approval that was just written"
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
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("status: draft"));
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
    use super::store_backed_drafts::{fixture_email, signature_logo_message, Fixture};
    use super::*;
    use crate::parse::AttachmentData;
    use crate::store::read::MessageRow;
    use crate::store::BlobStore;
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
            draft_sync: None,
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
            conv: None,
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
        assert_eq!(files.inline_from, 2, "no inline image: {files:?}");
        let files = files.files;

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

    /// The body's inline image stays reachable from `o` and `O`: it is
    /// materialised after the real attachments, which is where the picker
    /// tags it (PERSO-165).
    #[test]
    fn the_inline_images_follow_the_real_attachments() {
        let fx = Fixture::new();
        let row = fx.ingest_raw(signature_logo_message().as_bytes());
        let mut app = app_on_row(&fx, &row);

        let files = cursor_attachment_files(&mut app).unwrap();
        let names: Vec<String> = files
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["report.pdf", "photo.png", "image001.png"]);
        assert_eq!(files.inline_from, 2);
        assert_eq!(std::fs::read(&files.files[2]).unwrap(), b"logo-bytes");
    }

    /// A message with no attachments is not an error: the empty list is what
    /// the picker turns into "No attachments".
    #[test]
    fn a_row_without_attachments_resolves_to_an_empty_list() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Bare"));
        let mut app = app_on_row(&fx, &row);

        assert_eq!(
            cursor_attachment_files(&mut app),
            Some(crate::tui::app::AttachmentFiles::default())
        );
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
            draft_sync: None,
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
            conv: None,
        }
    }

    /// Write a draft naming `attachments` and index it, returning the app with
    /// the cursor on it.
    fn app_on_draft(fx: &Fixture, attachments: &[String]) -> App {
        let dir = crate::config::drafts_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        let listed: String = attachments
            .iter()
            .map(|a| format!("  - \"{a}\"\n"))
            .collect();
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

        assert_eq!(
            cursor_attachment_files(&mut app),
            Some(crate::tui::app::AttachmentFiles::real(vec![one, two]))
        );
        assert!(app.status_message.is_none(), "{:?}", app.status_message);
    }

    /// A draft with no `attachments:` is not an error: the empty list is what
    /// the picker turns into "No attachments", the same as a bare message.
    #[test]
    fn a_draft_without_attachments_resolves_to_an_empty_list() {
        let fx = Fixture::new();
        let mut app = app_on_draft(&fx, &[]);

        assert_eq!(
            cursor_attachment_files(&mut app),
            Some(crate::tui::app::AttachmentFiles::default())
        );
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
        assert_eq!(
            cursor_attachment_files(&mut app),
            Some(crate::tui::app::AttachmentFiles::real(vec![present]))
        );
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
        let files = cursor_attachment_files(&mut app).unwrap().files;

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
        assert!(
            written.starts_with("<meta http-equiv=\"Content-Security-Policy\""),
            "{written}"
        );
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

    /// `te` (PERSO-174): the message's own raw bytes, materialised as
    /// `<date>_<subject>.eml`, go into the directory picker `ts` opens, so the
    /// copy is the one a saved part takes; on a draft it says why not.
    #[test]
    fn te_hands_the_messages_eml_to_the_directory_picker() {
        let fx = Fixture::new();
        let raw = b"From: Alice <alice@example.com>\r\nTo: me@example.com\r\n\
            Subject: Quarterly report\r\nDate: Mon, 01 Jan 2024 12:00:00 +0000\r\n\
            Message-ID: <q@example.com>\r\n\r\nThe numbers.\r\n";
        let row = fx.ingest_raw(raw);
        let mut app = app_on_row(&fx, &row);

        press(&mut app, "te");

        let sources = match &app.overlay {
            crate::tui::app::Overlay::Dir(picker) => picker.sources.clone(),
            _ => panic!("te opened no directory picker: {:?}", app.status_message),
        };
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(
            sources[0].file_name().unwrap(),
            "2024-01-01_Quarterly report.eml"
        );
        assert_eq!(std::fs::read(&sources[0]).unwrap(), raw);
    }

    /// The search overlay's `te` and `cF` (PERSO-174) queue the hit's export
    /// and its forward as attachment, `e` still opens it, and the export of a
    /// server-only hit is the daemon's `.eml` for its Message-ID, here one the
    /// store has caught up with since the search.
    #[test]
    fn the_search_overlays_te_exports_a_server_only_hits_eml() {
        use crate::tui::app::{Action, Overlay, SearchOverlayFocus, SearchResultEntry};
        let fx = Fixture::new();
        let raw = b"From: Alice <alice@example.com>\r\nTo: me@example.com\r\n\
            Subject: Quarterly report\r\nDate: Mon, 01 Jan 2024 12:00:00 +0000\r\n\
            Message-ID: <q@example.com>\r\n\r\nThe numbers.\r\n";
        let row = fx.ingest_raw(raw);
        let mut app = app_on_row(&fx, &row);
        let mut entry = app.emails[0].clone();
        entry.msg = None;
        app.server_search_results = vec![SearchResultEntry {
            entry,
            fetched: crate::parse::parse_rfc822_to_fetched_email(raw).unwrap(),
            source_label: "Inbox".to_string(),
        }];
        app.server_search_index = 0;
        app.overlay = Overlay::Search;
        app.server_search_focus = SearchOverlayFocus::List;

        for (chord, want) in [
            ("te", Action::SearchResultExport),
            ("cF", Action::SearchResultForwardAsAttachment),
            ("e", Action::SearchResultOpen),
        ] {
            app.pending_actions.clear();
            press(&mut app, chord);
            let queued: Vec<String> = app
                .pending_actions
                .iter()
                .map(|a| format!("{a:?}"))
                .collect();
            assert_eq!(queued, [format!("{want:?}")], "{chord}");
        }

        let path = crate::tui::actions::search_result_eml_file(&mut app)
            .unwrap_or_else(|| panic!("no .eml: {:?}", app.server_search_status));
        assert_eq!(path.file_name().unwrap(), "2024-01-01_Quarterly report.eml");
        assert_eq!(std::fs::read(&path).unwrap(), raw);
    }

    /// The search overlay's `w` and `cF` open the forward wizard over a
    /// server-only hit, as the list's do over a row and the desktop's do over
    /// either, with the hit's own `Fwd:` subject; cancelling it goes back to
    /// the overlay with the results as they were (PERSO-155).
    #[test]
    fn the_search_overlays_forward_opens_the_wizard_and_cancel_returns_to_it() {
        use crate::tui::app::{
            ComposeMode, MessageAddress, Overlay, SearchOverlayFocus, SearchResultEntry,
        };
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Report"));
        let mut app = app_on_row(&fx, &row);
        let mut entry = app.emails[0].clone();
        entry.msg = None;
        let mut fetched = fixture_email("Report");
        fetched.message_id = Some("<server-only@example.com>".to_string());
        app.server_search_results = vec![SearchResultEntry {
            entry,
            fetched,
            source_label: "Sent".to_string(),
        }];
        app.server_search_index = 0;
        app.overlay = Overlay::Search;
        app.server_search_focus = SearchOverlayFocus::List;

        for as_attachment in [false, true] {
            crate::tui::actions::search_result_forward(&mut app, as_attachment);
            match &app.overlay {
                Overlay::Compose(wizard) => {
                    assert_eq!(wizard.subject, "Fwd: Report");
                    match &wizard.mode {
                        ComposeMode::SearchForward(forward) => {
                            assert_eq!(forward.as_attachment, as_attachment);
                            assert_eq!(
                                forward.address,
                                MessageAddress::Hit {
                                    message_id: "<server-only@example.com>".to_string(),
                                    mailbox: "Sent".to_string(),
                                }
                            );
                        }
                        other => panic!("not the search forward: {other:?}"),
                    }
                }
                _ => panic!("the forward wizard did not open"),
            }
            crate::tui::actions::cancel_compose_wizard(&mut app);
            assert!(
                matches!(app.overlay, Overlay::Search),
                "back to the overlay"
            );
            assert_eq!(app.server_search_results.len(), 1, "the results stay");
        }
    }

    /// `cf` and `cF` open the one forward wizard; `cF`'s carries the message
    /// as its `.eml` (PERSO-174).
    #[test]
    fn cf_and_cf_upper_open_the_forward_wizard_with_and_without_the_eml() {
        let fx = Fixture::new();
        let row = fx.ingest(&fixture_email("Report"));
        let mut app = app_on_row(&fx, &row);
        for (chord, attached) in [("cf", false), ("cF", true)] {
            app.pending_actions.clear();
            press(&mut app, chord);
            match app.pending_actions.back() {
                Some(crate::tui::app::Action::OpenComposeWizard(
                    crate::tui::app::ComposeMode::Forward { msg, as_attachment },
                )) => {
                    assert_eq!(msg.row_id(), row.id);
                    assert_eq!(*as_attachment, attached, "{chord}");
                }
                other => panic!("{chord} queued no forward wizard: {other:?}"),
            }
        }
    }

    /// An app on the Drafts mailbox with the cursor on the one draft `file`
    /// holds, and a session that records what it asks the daemon.
    fn app_on_drafts_row(
        fx: &Fixture,
        file: &str,
        id: &str,
    ) -> (App, super::super::daemon::CallLog) {
        let dir = crate::config::drafts_dir("alice");
        let store = fx.store();
        crate::store::drafts::refresh(&store, "alice", &dir).unwrap();
        assert!(dir.join(file).exists());

        let log = super::super::daemon::CallLog::default();
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.mailboxes = crate::tui::app::build_mailboxes(&app.account_config);
        app.active_mailbox = app
            .find_mailbox_by_kind(crate::tui::app::MailboxKind::Drafts)
            .expect("the Drafts mailbox is one of the four roles");
        app.session = Some(TestDaemon::new(&["alice"]).recording_session(log.clone()));
        app.emails = std::sync::Arc::new(vec![draft_entry(Some(id))]);
        app.rebuild_visible();
        app.list_index = 0;
        (app, log)
    }

    fn press(app: &mut App, chord: &str) {
        for c in chord.chars() {
            app.handle_key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Char(c),
            ));
        }
    }

    /// `tb` on a reply draft (PERSO-136): the Markdown file has no markup, so
    /// the key asks `draft.html` for the HTML part the send would build and
    /// hands the browser that, signature once and the companion's quoted
    /// original below it, hardened like a received message's rendition.
    #[test]
    fn tb_on_a_drafts_row_opens_the_html_the_send_would_build() {
        let fx = Fixture::new();
        let dir = crate::config::drafts_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("re-hello.md"),
            format!(
                "---\nid: re0000000000001\nto: bob@example.com\nsubject: \"Re: Hello\"\n\
                 status: draft\n---\n\nDanke, passt.\n\n{}\nGruss aus dem Test\n{}\n\n{}\n\n\
                 > hello there\n",
                crate::draft::SIG_START,
                crate::draft::SIG_END,
                crate::draft::QUOTE_MARKER,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("re-hello.html"),
            "<p style=\"color:#666\">On Monday, bob wrote:</p>\n<div><p>the original markup</p></div>",
        )
        .unwrap();
        let (mut app, log) = app_on_drafts_row(&fx, "re-hello.md", "re0000000000001");

        press(&mut app, "tb");

        let calls = log.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(
                "draft.html".to_string(),
                serde_json::json!({"account": "alice", "id": "re0000000000001"})
            )],
            "tb on a draft asks draft.html and nothing else"
        );
        let path = match app.pending_actions.back() {
            Some(crate::tui::app::Action::OpenHtmlInBrowser(path)) => path.clone(),
            other => panic!("tb queued no browser open: {other:?}"),
        };
        assert_eq!(path.extension().unwrap(), "html");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("Danke, passt."), "{written}");
        assert_eq!(
            written.matches("Gruss aus dem Test").count(),
            1,
            "{written}"
        );
        assert!(written.contains("the original markup"), "{written}");
        assert!(
            !written.contains("hello there"),
            "the companion replaces the Markdown quote, as on the send: {written}"
        );
        assert!(
            written.contains("<meta http-equiv=\"Content-Security-Policy\""),
            "{written}"
        );
        assert!(written.contains("<meta charset=\"UTF-8\">"), "{written}");
        assert!(app.status_message.is_none(), "{:?}", app.status_message);
    }

    /// A draft the daemon cannot find says so on the status line and opens
    /// nothing, rather than the received-message line about missing markup.
    #[test]
    fn tb_on_a_vanished_draft_says_the_preview_failed() {
        let fx = Fixture::new();
        let dir = crate::config::drafts_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("gone.md"),
            "---\nid: gone000000000001\nto: bob@example.com\nsubject: Gone\nstatus: draft\n---\n\nBody\n",
        )
        .unwrap();
        let (mut app, log) = app_on_drafts_row(&fx, "gone.md", "gone000000000001");
        std::fs::remove_file(dir.join("gone.md")).unwrap();

        press(&mut app, "tb");

        assert_eq!(log.lock().unwrap().len(), 1, "one draft.html call");
        assert!(
            !app.pending_actions
                .iter()
                .any(|a| matches!(a, crate::tui::app::Action::OpenHtmlInBrowser(_))),
            "nothing to open"
        );
        let status = app.status_message.clone().unwrap();
        assert!(status.starts_with("Draft preview failed: "), "{status}");
    }

    /// The server-search hit that resolved to no local row: its markup is the
    /// bytes the overlay is already holding, written out so the browser sees a
    /// file either way.
    #[test]
    fn an_unresolved_search_hits_markup_is_served_from_the_fetch() {
        let _fx = Fixture::new();
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        let fetched = fixture_email("Never synced");
        let html = fetched.html_body.clone().unwrap();
        let page = html_rendition(&mut app, &html, "search-7").unwrap();
        let page_html = std::fs::read_to_string(&page).unwrap();
        assert!(page_html.ends_with("<p>Rich body</p>"), "{page_html}");
        assert!(page_html.contains("Content-Security-Policy"), "{page_html}");
    }

    /// `o`, `O`, `e` and `y` on a server-only hit ask the daemon with the
    /// hit's address, as `te` does, where they used to decline or write the
    /// attachments of the streamed hit, which carries none: the daemon answers
    /// from the server's copy, here the row the store has caught up with since
    /// the search (PERSO-155).
    #[test]
    fn a_server_only_hits_attachments_and_view_come_from_the_daemon() {
        use crate::tui::app::{MessageAddress, Overlay, SearchOverlayFocus, SearchResultEntry};
        let fx = Fixture::new();
        let raw = b"From: Alice <alice@example.com>\r\nTo: me@example.com\r\n\
            Subject: LOC Day\r\nDate: Wed, 12 Nov 2025 09:00:00 +0100\r\n\
            Message-ID: <loc-day@example.com>\r\nMIME-Version: 1.0\r\n\
            Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
            --b\r\nContent-Type: text/plain\r\n\r\nThe list.\r\n\
            --b\r\nContent-Type: application/vnd.ms-excel\r\n\
            Content-Disposition: attachment; filename=\"teilnehmer.xlsx\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\nUEsDBA==\r\n--b--\r\n";
        let row = fx.ingest_raw(raw);
        let mut app = app_on_row(&fx, &row);
        let mut entry = app.emails[0].clone();
        entry.msg = None;
        // The streamed hit carries no parts, as `message.server_hit` sends it.
        let mut fetched = crate::parse::parse_rfc822_to_fetched_email(raw).unwrap();
        fetched.attachments.clear();
        app.server_search_results = vec![SearchResultEntry {
            entry,
            fetched,
            source_label: "Sent".to_string(),
        }];
        app.server_search_index = 0;
        app.overlay = Overlay::Search;
        app.server_search_focus = SearchOverlayFocus::List;

        let address = crate::tui::actions::selected_hit_address(&mut app).unwrap();
        assert_eq!(
            address,
            MessageAddress::Hit {
                message_id: "<loc-day@example.com>".to_string(),
                mailbox: "Sent".to_string(),
            }
        );
        let files = crate::tui::actions::search_hit_attachment_files(&mut app)
            .unwrap_or_else(|| panic!("no files: {:?}", app.server_search_status));
        assert_eq!(files.files.len(), 1, "{files:?}");
        assert_eq!(files.files[0].file_name().unwrap(), "teilnehmer.xlsx");

        let view = crate::tui::actions::readonly_view(&mut app, &address)
            .unwrap_or_else(|| panic!("no view: {:?}", app.server_search_status));
        let text = std::fs::read_to_string(&view.path).unwrap();
        assert!(text.contains("LOC Day"), "{text}");
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
        assert!(
            content.contains("subject: 'Quarterly Report: Q3'\n"),
            "{content}"
        );
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
        assert!(
            finish_readonly_view(&mut app, &rendition, Ok(())),
            "a clean exit is what returns the keyboard to the list (PERSO-124)"
        );
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
        assert!(!finish_readonly_view(
            &mut app,
            &rendition,
            Err(anyhow::anyhow!("editor exited with 1")),
        ));
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
