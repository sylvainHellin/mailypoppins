//! Reads the account's message rows and builds a `ContactIndex`.
//!
//! The full rebuild reads `messages` through `store::read`, the same listing
//! shape the TUI and `mp dump-mailbox` use: every row already carries `from`,
//! `to`, `cc`, `mailbox` and the `Date:` header, which is exactly what the
//! ranker needs. Before [#0053](../../docs/tickets/0053-contacts-rebuild-data-loss.md)
//! this walked a `.md` tree that the store cutover deleted, so a rebuild found
//! nothing and the caller cached the nothing over months of accumulated
//! frecency.
//!
//! The observation half - the `ObservedIn` hook kinds, the header parse, the
//! per-observation merge - reads nothing and lives in
//! `mp_core::contacts::extractor` since #0126 (P5-U10b). This module is the
//! store reader and calls back into it.

use crate::config::AccountConfig;
use crate::contacts::types::ContactIndex;
use crate::store::read::{self, MessageRow};
use crate::store::{open_store, Store};
use crate::types::MailboxRole;
use anyhow::Result;
use mp_core::contacts::extractor::{
    empty_index, parse_date_to_rfc3339, process_header, self_address, UNDATED_OBSERVED_AT,
};
use mp_core::contacts::rank::ObservationField;

/// Build a full `ContactIndex` from the account's message store.
///
/// An account that has never synced has no store, and that is not an error:
/// the index comes back empty and the caller decides what to do with it (see
/// `cache::save_rebuilt_cache`, which refuses to persist an empty rebuild over
/// a populated cache).
pub fn build_index_for_account(account: &AccountConfig) -> Result<ContactIndex> {
    match open_store(&account.name) {
        Some(store) => build_index_from_store(&store, account),
        None => Ok(empty_index(account)),
    }
}

/// Build a full `ContactIndex` from an already-open store.
pub(crate) fn build_index_from_store(
    store: &Store,
    account: &AccountConfig,
) -> Result<ContactIndex> {
    let mut index = empty_index(account);
    let self_addr = self_address(account);

    for row in read::list_account(store, &account.name)? {
        let role = MailboxRole::from(row.mailbox.as_str());
        let observed_at = row
            .date_display
            .as_deref()
            .and_then(parse_date_to_rfc3339)
            .unwrap_or_else(|| UNDATED_OBSERVED_AT.to_string());
        for (field, raw) in header_fields(&row) {
            let Some(raw) = raw else { continue };
            if raw.trim().is_empty() {
                continue;
            }
            process_header(
                &mut index.contacts,
                raw,
                field,
                &role,
                &observed_at,
                &self_addr,
            );
        }
    }

    Ok(index)
}

/// The from/to/cc headers of one row, in the order the ranker sees them.
fn header_fields(row: &MessageRow) -> [(ObservationField, Option<&str>); 3] {
    [
        (ObservationField::From, row.from.as_deref()),
        (ObservationField::To, row.to.as_deref()),
        (ObservationField::Cc, row.cc.as_deref()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{ingest_message, IngestInput};
    use crate::parse::FetchedEmail;
    use crate::store::BlobStore;
    use tempfile::TempDir;

    /// A store plus its blob store, both under one temp directory. No mailbox
    /// tree exists anywhere near it: the rebuild reads rows only.
    struct Fixture {
        _dir: TempDir,
        store: Store,
        blobs: BlobStore,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Fixture {
            _dir: dir,
            store,
            blobs,
        }
    }

    fn account() -> AccountConfig {
        AccountConfig {
            name: "alice".into(),
            default_from: "me@example.com".into(),
            ..Default::default()
        }
    }

    fn email(from: &str, to: &str, cc: Option<&str>, date: &str) -> FetchedEmail {
        FetchedEmail {
            from: from.into(),
            to: to.into(),
            cc: cc.map(|s| s.into()),
            reply_to: None,
            bcc: None,
            subject: "Subject".into(),
            date: date.into(),
            body_text: "body".into(),
            html_body: None,
            has_attachments: false,
            message_id: Some(format!("<{from}-{date}@example.com>")),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        }
    }

    /// Ingest through the real ingest API, so the fixture rows are exactly the
    /// rows the sync path writes.
    fn ingest(fx: &Fixture, mailbox: &str, uid: i64, email: &FetchedEmail) {
        ingest_message(
            &fx.store,
            &fx.blobs,
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

    /// #0053: the rebuild reads the store, so two ingested messages produce
    /// two contacts with no mailbox tree present.
    #[test]
    fn rebuild_finds_both_senders_of_a_two_message_store() {
        let fx = fixture();
        ingest(
            &fx,
            "inbox",
            1,
            &email(
                "Alice <alice@example.com>",
                "me@example.com",
                None,
                "Mon, 05 Jan 2026 12:00:00 +0000",
            ),
        );
        ingest(
            &fx,
            "inbox",
            2,
            &email(
                "Bob <bob@example.com>",
                "me@example.com",
                None,
                "Tue, 06 Jan 2026 12:00:00 +0000",
            ),
        );

        let index = build_index_from_store(&fx.store, &account()).unwrap();

        assert_eq!(index.contacts.len(), 2);
        let alice = index.contacts.get("alice@example.com").expect("alice");
        assert_eq!(alice.display_name, "Alice");
        assert_eq!(alice.received, 1);
        assert_eq!(alice.sent_to, 0);
        assert_eq!(alice.last_seen, "2026-01-05T12:00:00+00:00");
        let bob = index.contacts.get("bob@example.com").expect("bob");
        assert_eq!(bob.received, 1);
        // The self address is filtered out of every field.
        assert!(!index.contacts.contains_key("me@example.com"));
    }

    /// The role comes from the row's `mailbox` column: a `sent` row bumps
    /// sent_to/sent_cc, an archive row counts as received.
    #[test]
    fn the_row_mailbox_decides_the_observation_role() {
        let fx = fixture();
        ingest(
            &fx,
            "sent",
            1,
            &email(
                "me@example.com",
                "Carol <carol@example.com>",
                Some("Dave <dave@example.com>"),
                "Wed, 07 Jan 2026 12:00:00 +0000",
            ),
        );
        ingest(
            &fx,
            "archive",
            1,
            &email(
                "Erin <erin@example.com>",
                "me@example.com",
                None,
                "Thu, 08 Jan 2026 12:00:00 +0000",
            ),
        );

        let index = build_index_from_store(&fx.store, &account()).unwrap();

        assert_eq!(index.contacts.get("carol@example.com").unwrap().sent_to, 1);
        assert_eq!(index.contacts.get("dave@example.com").unwrap().sent_cc, 1);
        assert_eq!(index.contacts.get("erin@example.com").unwrap().received, 1);
    }

    /// A store with no rows for this account builds an empty index rather than
    /// failing; the caller's guard decides what that means.
    #[test]
    fn an_empty_store_builds_an_empty_index() {
        let fx = fixture();
        let index = build_index_from_store(&fx.store, &account()).unwrap();
        assert!(index.contacts.is_empty());
        assert_eq!(index.account, "alice");
    }

    /// #0067: an unparseable `Date:` gets a constant `observed_at`, so two
    /// rebuilds of the same undated mail produce byte-identical contacts.
    #[test]
    fn undated_mail_rebuilds_to_the_same_index_every_time() {
        let fx = fixture();
        ingest(
            &fx,
            "inbox",
            1,
            &email("Frank <frank@example.com>", "me@example.com", None, ""),
        );

        let first = build_index_from_store(&fx.store, &account()).unwrap();
        let second = build_index_from_store(&fx.store, &account()).unwrap();

        let frank = first.contacts.get("frank@example.com").expect("frank");
        assert_eq!(frank.last_seen, UNDATED_OBSERVED_AT);
        assert_eq!(frank.first_seen, UNDATED_OBSERVED_AT);
        assert_eq!(
            serde_json::to_string(&first.contacts).unwrap(),
            serde_json::to_string(&second.contacts).unwrap()
        );
    }

    /// #0067: `default_from` written as `Name <addr>` still filters the
    /// user's own address out of their own corpus.
    #[test]
    fn a_display_name_default_from_still_filters_self() {
        let fx = fixture();
        let mut account = account();
        account.default_from = "Me Myself <ME@example.com>".into();
        ingest(
            &fx,
            "inbox",
            1,
            &email(
                "Alice <alice@example.com>",
                "me@example.com",
                None,
                "Mon, 05 Jan 2026 12:00:00 +0000",
            ),
        );

        let index = build_index_from_store(&fx.store, &account).unwrap();

        assert!(!index.contacts.contains_key("me@example.com"));
        assert!(index.contacts.contains_key("alice@example.com"));
    }

    /// #0067: the account-level entry point, whose missing-store branch the
    /// store-level tests never reach. An account that has never synced has no
    /// `store.db`, and that is an empty index, not an error.
    #[test]
    fn an_account_with_no_store_builds_an_empty_index() {
        let _tmp = crate::config::test_env::TestDataDir::new();

        let index = build_index_for_account(&account()).unwrap();

        assert!(index.contacts.is_empty());
        assert_eq!(index.account, "alice");
    }

    /// The `messages.mailbox` value reads back as its role, and an unmapped
    /// mailbox keeps its name; only `sent` changes the ranking, so every other
    /// role counts as received (#0064).
    #[test]
    fn a_stored_mailbox_value_reads_back_as_its_role() {
        assert_eq!(MailboxRole::from("sent"), MailboxRole::Sent);
        assert_eq!(MailboxRole::from("inbox"), MailboxRole::Inbox);
        assert_eq!(MailboxRole::from("archive"), MailboxRole::Archive);
        assert_eq!(
            MailboxRole::from("some-folder"),
            MailboxRole::Other("some-folder".to_string())
        );
    }
}
