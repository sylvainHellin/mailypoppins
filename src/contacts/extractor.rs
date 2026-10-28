//! Reads the account's message rows and builds a `ContactIndex`.
//!
//! The full rebuild reads `messages` through `store::read`, the same listing
//! shape the TUI and `mp dump-mailbox` use: every row already carries `from`,
//! `to`, `cc`, `mailbox` and the `Date:` header, which is exactly what the
//! ranker needs. Before #0053
//! this walked a `.md` tree that the store cutover deleted, so a rebuild found
//! nothing and the caller cached the nothing over months of accumulated
//! frecency.
//!
//! The observation half - the header parse, the per-observation merge - reads
//! nothing and lives in `mp_core::contacts::extractor` since #0126 (P5-U10b).
//! This module is the store reader, and it folds each row in through
//! [`observe_message`], the rule the send and sync hooks apply too
//! (PERSO-234), so what a rebuild indexes the hooks index.

use std::path::Path;

use crate::config::AccountConfig;
use crate::contacts::hooks::fold_observation;
use crate::contacts::types::ContactIndex;
use crate::contacts::window::{self, RebuildWindow};
use crate::store::read::{self, MessageRow};
use crate::store::{open_store_in, Store};
use crate::types::MailboxRole;
use anyhow::Result;
use mp_core::contacts::extractor::{empty_index, observe_message, MessageAddresses};
use mp_core::contacts::{
    load_cache, save_cache, save_rebuilt_cache, with_cache_lock, CacheSave, EXTRACTOR_VERSION,
};

/// Build a full `ContactIndex` from the account's message store.
///
/// An account that has never synced has no store, and that is not an error:
/// the index comes back empty and the caller decides what to do with it (see
/// `cache::save_rebuilt_cache`, which refuses to persist an empty rebuild over
/// a populated cache).
pub fn build_index_for_account(account: &AccountConfig) -> Result<ContactIndex> {
    build_index_for_account_until(account, || false)
        .map(|index| index.expect("a walk nothing stops always finishes"))
}

/// [`build_index_for_account`], asking `stop` every [`STOP_EVERY_ROWS`] rows
/// whether to give up, which is how `contact.rebuild` observes its cancel token
/// inside the walk. `None` is a walk that was stopped: the half-built index is
/// dropped, because a partial ranking is not something any caller may cache.
pub fn build_index_for_account_until(
    account: &AccountConfig,
    stop: impl Fn() -> bool,
) -> Result<Option<ContactIndex>> {
    build_index_in(&crate::config::account_dir(&account.name), account, stop)
}

/// [`build_index_for_account_until`] over the store in the account directory
/// `root`, which a caller running this on a blocking thread resolves first.
pub fn build_index_in(
    root: &Path,
    account: &AccountConfig,
    stop: impl Fn() -> bool,
) -> Result<Option<ContactIndex>> {
    match open_store_in(root) {
        Some(store) => build_index_from_store_until(&store, account, stop),
        None => Ok(Some(empty_index(account))),
    }
}

/// How many rows the walk processes between two `stop` checks: the per-row
/// work is a header parse, so a check per row would be noise and one per walk
/// would not be a check at all.
pub const STOP_EVERY_ROWS: usize = 256;

/// Build a full `ContactIndex` from an already-open store.
#[cfg(test)]
fn build_index_from_store(store: &Store, account: &AccountConfig) -> Result<ContactIndex> {
    build_index_from_store_until(store, account, || false)
        .map(|index| index.expect("a walk nothing stops always finishes"))
}

/// The walk itself, stoppable between two batches of [`STOP_EVERY_ROWS`].
fn build_index_from_store_until(
    store: &Store,
    account: &AccountConfig,
    stop: impl Fn() -> bool,
) -> Result<Option<ContactIndex>> {
    let mut index = empty_index(account);
    let self_addr = account.own_address();

    for (n, row) in read::list_account(store, &account.name)?
        .into_iter()
        .enumerate()
    {
        if n % STOP_EVERY_ROWS == 0 && stop() {
            return Ok(None);
        }
        observe_message(
            &mut index.contacts,
            &self_addr,
            &MailboxRole::from(row.mailbox.as_str()),
            &row_addresses(&row),
            row.date_display.as_deref(),
        );
    }

    Ok(Some(index))
}

/// The address headers of one row.
fn row_addresses(row: &MessageRow) -> MessageAddresses<'_> {
    MessageAddresses {
        from: row.from.as_deref(),
        to: row.to.as_deref(),
        cc: row.cc.as_deref(),
        bcc: row.bcc.as_deref(),
    }
}

/// What committing a rebuild did, and the index the caller serves after it.
#[derive(Debug)]
pub struct Committed {
    /// The verdict of the cache guard (#0067).
    pub saved: CacheSave,
    /// The rebuilt index when it was written; the cache the guard kept when it
    /// refused, or the rebuilt index when there was nothing to keep.
    pub index: ContactIndex,
}

/// Offer a rebuilt `index` to the cache under the account directory `root`,
/// holding the cache's lock from the catch-up to the save (PERSO-234).
///
/// `window` is the one the caller opened before the walk listed the store
/// ([`RebuildWindow::open`]). The walk ran without the lock, and a sync may
/// have inserted rows and merged them into the cache this commit replaces;
/// those merges logged their observations into the window, and they are
/// folded into the rebuilt index here. Once it is written, a merge arriving
/// late for a row the walk saw is dropped ([`window::committed`]).
///
/// `stamp_kept` is for the reads that rebuild a stale cache: when the guard
/// refuses the rebuild, the kept cache is stamped current, so the next read
/// does not walk the store again to reach the same verdict, and whatever the
/// window logged that never reached it (a stale cache takes no merge) is
/// folded into it first.
pub fn commit_rebuild_at(
    root: &Path,
    account: &AccountConfig,
    mut index: ContactIndex,
    window: RebuildWindow,
    stamp_kept: bool,
) -> Result<Committed> {
    with_cache_lock(root, || {
        let (mark, logged) = window.close();
        let self_addr = account.own_address();
        for entry in &logged {
            fold_observation(&mut index, &self_addr, &entry.observation);
        }
        let saved = save_rebuilt_cache(root, &index)?;
        if saved == CacheSave::Written {
            window::committed(root, mark);
            return Ok(Committed { saved, index });
        }
        let index = match load_cache(root)? {
            Some(mut kept) => {
                let stamp = stamp_kept && !kept.is_current();
                if stamp {
                    kept.extractor_version = EXTRACTOR_VERSION;
                }
                let unapplied: Vec<_> = logged.iter().filter(|entry| !entry.applied).collect();
                let fold = kept.is_current() && !unapplied.is_empty();
                if fold {
                    for entry in unapplied {
                        fold_observation(&mut kept, &self_addr, &entry.observation);
                    }
                }
                if stamp || fold {
                    save_cache(root, &kept)?;
                }
                kept
            }
            None => index,
        };
        Ok(Committed { saved, index })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{ingest_message, IngestInput};
    use crate::parse::FetchedEmail;
    use crate::store::BlobStore;
    use mp_core::contacts::extractor::UNDATED_OBSERVED_AT;
    use mp_core::contacts::save_cache;
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
            server_has_attachments: None,
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

    /// A walk whose `stop` answers yes gives the index up rather than handing
    /// back a partial ranking, and one that never stops is the plain build.
    #[test]
    fn a_stopped_walk_returns_no_index() {
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

        let stopped = build_index_from_store_until(&fx.store, &account(), || true).unwrap();
        assert!(stopped.is_none(), "a stopped walk is not an index");
        let finished = build_index_from_store_until(&fx.store, &account(), || false)
            .unwrap()
            .expect("a walk nothing stops finishes");
        assert_eq!(finished.contacts.len(), 1);
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

    /// Ingest one message the way a sync does, and answer the observation the
    /// engine records for the row it inserted.
    fn sync_in(fx: &Fixture, uid: i64, from: &str) -> crate::sync::FreshObservation {
        let message = email(
            from,
            "me@example.com",
            None,
            "Wed, 07 Jan 2026 12:00:00 +0000",
        );
        let outcome = ingest_message(
            &fx.store,
            &fx.blobs,
            &IngestInput {
                account: "alice",
                mailbox: "inbox",
                uid,
                email: &message,
                raw: None,
            },
        )
        .unwrap();
        assert!(outcome.inserted);
        crate::sync::FreshObservation::of(&MailboxRole::Inbox, &message)
    }

    fn received(root: &Path, address: &str) -> u32 {
        let cached = load_cache(root).unwrap().expect("cached");
        cached.contacts.get(address).map_or(0, |c| c.received)
    }

    /// PERSO-234 review: a sync that inserts a row after a rebuild's walk and
    /// merges it into the old cache before the rebuild commits must not lose
    /// it when the rebuild replaces that cache: the commit folds in what the
    /// walk did not see.
    #[test]
    fn a_rebuild_keeps_a_row_a_sync_inserted_during_its_walk() {
        let fx = fixture();
        let root = fx._dir.path();
        ingest(
            &fx,
            "inbox",
            1,
            &email("Alice <alice@example.com>", "me@example.com", None, ""),
        );
        let first = build_index_from_store(&fx.store, &account()).unwrap();
        save_cache(root, &first).unwrap();

        let (walked, window) = walk(&fx);
        let carol = sync_in(&fx, 2, "Carol <carol@example.com>");
        crate::contacts::hooks::bump_after_sync_at(root, &account(), &[carol]);
        let committed = commit_rebuild_at(root, &account(), walked, window, false).unwrap();

        assert_eq!(committed.saved, CacheSave::Written);
        assert_eq!(received(root, "carol@example.com"), 1, "kept, once");
        assert_eq!(received(root, "alice@example.com"), 1);
    }

    /// And a sync whose merge waited for the commit, for a row the walk had
    /// already counted, does not count it twice.
    #[test]
    fn a_merge_after_a_rebuild_skips_a_row_the_rebuild_counted() {
        let fx = fixture();
        let root = fx._dir.path();
        ingest(
            &fx,
            "inbox",
            1,
            &email("Alice <alice@example.com>", "me@example.com", None, ""),
        );
        save_cache(
            root,
            &build_index_from_store(&fx.store, &account()).unwrap(),
        )
        .unwrap();

        let carol = sync_in(&fx, 2, "Carol <carol@example.com>");
        let (walked, window) = walk(&fx);
        commit_rebuild_at(root, &account(), walked, window, false).unwrap();
        let late = crate::contacts::hooks::bump_after_sync_at(root, &account(), &[carol]);

        assert_eq!(late, None, "nothing left to merge");
        assert_eq!(received(root, "carol@example.com"), 1, "counted once");

        let dave = sync_in(&fx, 3, "Dave <dave@example.com>");
        assert!(crate::contacts::hooks::bump_after_sync_at(root, &account(), &[dave]).is_some());
        assert_eq!(
            received(root, "dave@example.com"),
            1,
            "a later row still merges"
        );
    }

    /// The first read after an upgrade rebuilds a stale cache, which takes no
    /// merge: a row a sync inserts during that walk reaches the rebuilt index
    /// through the window's log all the same. A cancelled rebuild's window
    /// closes without touching what the next one decides.
    #[test]
    fn a_rebuild_of_a_stale_cache_keeps_a_row_synced_during_its_walk() {
        let fx = fixture();
        let root = fx._dir.path();
        ingest(
            &fx,
            "inbox",
            1,
            &email("Alice <alice@example.com>", "me@example.com", None, ""),
        );
        let mut stale = build_index_from_store(&fx.store, &account()).unwrap();
        stale.extractor_version = 0;
        save_cache(root, &stale).unwrap();

        drop(walk(&fx));
        let walked = walk(&fx);
        let carol = sync_in(&fx, 2, "Carol <carol@example.com>");
        let merged = crate::contacts::hooks::bump_after_sync_at(root, &account(), &[carol]);
        assert_eq!(merged, None, "a stale cache takes no merge");
        rebuild_and_commit(root, walked);

        assert_eq!(received(root, "carol@example.com"), 1);
        assert_eq!(received(root, "alice@example.com"), 1);
    }

    /// Delete the account's newest row, the way a user's delete or a sync's
    /// prune does, and answer its id, which SQLite hands the next insert:
    /// `messages.id` is an `INTEGER PRIMARY KEY` without `AUTOINCREMENT`.
    fn delete_newest(fx: &Fixture) -> i64 {
        let newest: i64 = fx
            .store
            .conn()
            .query_row("SELECT MAX(id) FROM messages", [], |r| r.get(0))
            .unwrap();
        crate::store::write::delete_row(&fx.store, &fx.blobs, newest)
            .unwrap()
            .expect("the newest row was there");
        newest
    }

    /// PERSO-234 review: a message from a new correspondent synced after a
    /// rebuild is searchable without another one, even when it lands on the id
    /// of a row deleted since, which SQLite reuses.
    #[test]
    fn a_merge_after_a_rebuild_keeps_a_row_on_a_reused_id() {
        let fx = fixture();
        let root = fx._dir.path();
        ingest(
            &fx,
            "inbox",
            1,
            &email("Alice <alice@example.com>", "me@example.com", None, ""),
        );
        ingest(
            &fx,
            "inbox",
            2,
            &email("Bob <bob@example.com>", "me@example.com", None, ""),
        );
        save_cache(
            root,
            &build_index_from_store(&fx.store, &account()).unwrap(),
        )
        .unwrap();
        rebuild_and_commit(root, walk(&fx));

        let freed = delete_newest(&fx);
        let xavier = sync_in(&fx, 3, "Xavier <xavier@example.com>");
        assert_eq!(xavier_row(&fx), freed, "the insert reused the freed id");
        crate::contacts::hooks::bump_after_sync_at(root, &account(), &[xavier]);

        assert_eq!(
            received(root, "xavier@example.com"),
            1,
            "searchable without a rebuild"
        );
    }

    /// The same reuse between a rebuild's walk and its commit: the merge lands
    /// in the cache the commit replaces, so the commit must carry the row over.
    #[test]
    fn a_rebuild_keeps_a_row_on_an_id_reused_during_its_walk() {
        let fx = fixture();
        let root = fx._dir.path();
        ingest(
            &fx,
            "inbox",
            1,
            &email("Alice <alice@example.com>", "me@example.com", None, ""),
        );
        ingest(
            &fx,
            "inbox",
            2,
            &email("Bob <bob@example.com>", "me@example.com", None, ""),
        );
        save_cache(
            root,
            &build_index_from_store(&fx.store, &account()).unwrap(),
        )
        .unwrap();

        let walked = walk(&fx);
        let freed = delete_newest(&fx);
        let xavier = sync_in(&fx, 3, "Xavier <xavier@example.com>");
        assert_eq!(xavier_row(&fx), freed, "the insert reused the freed id");
        crate::contacts::hooks::bump_after_sync_at(root, &account(), &[xavier]);
        rebuild_and_commit(root, walked);

        assert_eq!(received(root, "xavier@example.com"), 1, "kept, once");
    }

    fn xavier_row(fx: &Fixture) -> i64 {
        fx.store
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE from_ LIKE '%xavier%'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// A rebuild's walk, under the window it opened first, as every rebuild
    /// in the daemon runs it.
    fn walk(fx: &Fixture) -> (ContactIndex, RebuildWindow) {
        let window = RebuildWindow::open(fx._dir.path());
        let index = build_index_from_store(&fx.store, &account()).unwrap();
        (index, window)
    }

    fn rebuild_and_commit(root: &Path, (index, window): (ContactIndex, RebuildWindow)) {
        let committed = commit_rebuild_at(root, &account(), index, window, false).unwrap();
        assert_eq!(committed.saved, CacheSave::Written);
    }
}
