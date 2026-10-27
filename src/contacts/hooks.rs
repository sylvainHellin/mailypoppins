//! Best-effort hooks that keep the contacts index fresh after send/sync.
//!
//! Both fold a message in through [`observe_message`], the rule the full
//! rebuild applies to every row (PERSO-234), so an index kept up by the hooks
//! holds what a rebuild of the same mail would.
//!
//! Both are no-ops when the account has no `contacts-cache.json` yet, or has
//! one another extractor version built ([`ContactIndex::is_current`]): the first
//! `contact.search` or `contact.stats` rebuilds such a cache from the store,
//! which already holds the rows the hook would have merged.
//!
//! Every merge is one load-merge-save under the cache's lock, the lock a
//! rebuild's commit takes too ([`crate::contacts::commit_rebuild_at`]): that
//! commit folds in every row inserted after its walk before it saves, and
//! stamps the index with the highest row it folded
//! ([`ContactIndex::rows_through`]), so a sync merge that waited for it skips
//! the rows it already counted. A rebuild racing a sync neither loses a row
//! nor counts one twice.
//! All errors are logged at warning level but never propagated, so a broken
//! contacts index never fails a send or sync.

use std::path::Path;

use crate::config::{account_dir, AccountConfig};
use crate::contacts::{
    load_cache, observe_message, save_cache, with_cache_lock, ContactIndex, MessageAddresses,
};
use crate::sync::FreshObservation;
use crate::types::{EmailDraft, MailboxRole};
use chrono::Utc;
use log::{debug, warn};

/// Merge the recipients of a just-sent draft into the account's contact index.
/// Answers how many contacts the index holds once it was written, `None` when
/// nothing was written, so the daemon can announce the change.
///
/// The draft is the Sent copy the rebuild will later read back: its `From:`,
/// `To:`, `Cc:` and `Bcc:` under the `sent` role, observed now.
pub fn bump_after_send(account: &AccountConfig, draft: &EmailDraft) -> Option<usize> {
    let front = &draft.frontmatter;
    let addresses = MessageAddresses {
        from: front.from.as_deref(),
        to: front.to.as_deref(),
        cc: front.cc.as_deref(),
        bcc: front.bcc.as_deref(),
    };
    let blank = |h: Option<&str>| h.is_none_or(|h| h.trim().is_empty());
    if blank(addresses.to) && blank(addresses.cc) && blank(addresses.bcc) {
        return None;
    }
    let now = Utc::now().to_rfc3339();
    merge(
        &account_dir(&account.name),
        account,
        "send",
        |index, self_addr| {
            observe_message(
                &mut index.contacts,
                self_addr,
                &MailboxRole::Sent,
                &addresses,
                Some(&now),
            );
            1
        },
    )
}

/// Merge the messages a sync just inserted into the account's contact index.
/// Answers how many contacts the index holds once it was written, `None` when
/// nothing was written, so the daemon can announce the change.
pub fn bump_after_sync(
    account: &AccountConfig,
    observations: &[FreshObservation],
) -> Option<usize> {
    bump_after_sync_at(&account_dir(&account.name), account, observations)
}

/// [`bump_after_sync`] off the async runtime: the cache is a blocking read and
/// write, and its lock may be held by a rebuild's commit. The account
/// directory is resolved first, on the caller's thread.
pub async fn bump_after_sync_off_reactor(
    account: &AccountConfig,
    observations: Vec<FreshObservation>,
) -> Option<usize> {
    if observations.is_empty() {
        return None;
    }
    let root = account_dir(&account.name);
    let cfg = account.clone();
    tokio::task::spawn_blocking(move || bump_after_sync_at(&root, &cfg, &observations))
        .await
        .unwrap_or_else(|e| {
            warn!("contacts hook: the sync bump of '{}' {e}", account.name);
            None
        })
}

/// [`bump_after_sync`] against the account directory `root`, which a caller
/// that runs it off its own thread resolves first.
pub fn bump_after_sync_at(
    root: &Path,
    account: &AccountConfig,
    observations: &[FreshObservation],
) -> Option<usize> {
    if observations.is_empty() {
        return None;
    }
    merge(root, account, "sync", |index, self_addr| {
        // A row at or below the watermark is one the rebuild that wrote this
        // cache walked: it raced this sync and has counted it already.
        let fresh: Vec<&FreshObservation> = observations
            .iter()
            .filter(|obs| obs.row_id > index.rows_through)
            .collect();
        for obs in &fresh {
            observe_message(
                &mut index.contacts,
                self_addr,
                &obs.role,
                &MessageAddresses {
                    from: Some(obs.from.as_str()),
                    to: Some(obs.to.as_str()),
                    cc: obs.cc.as_deref(),
                    bcc: obs.bcc.as_deref(),
                },
                Some(obs.date.as_str()),
            );
        }
        fresh.len()
    })
}

/// Load the cache under `root`, let `fold` merge into it, and write it back,
/// all under the cache's lock ([`with_cache_lock`]), so no other merge and no
/// rebuild commit lands between the load and the save (PERSO-234).
/// `fold` answers how many messages it merged. `None` for a missing, stale or
/// unreadable cache, for a fold that merged nothing, and for a write that
/// failed.
fn merge(
    root: &Path,
    account: &AccountConfig,
    what: &str,
    fold: impl FnOnce(&mut ContactIndex, &str) -> usize,
) -> Option<usize> {
    with_cache_lock(root, || {
        let mut index = match load_cache(root) {
            Ok(Some(index)) if index.is_current() => index,
            Ok(Some(index)) => {
                debug!(
                    "contacts hook: the cache for '{}' is from extractor version {}, skipping the {what} bump until a read rebuilds it",
                    account.name, index.extractor_version
                );
                return None;
            }
            Ok(None) => {
                debug!(
                    "contacts hook: no cache for account '{}', skipping the {what} bump",
                    account.name
                );
                return None;
            }
            Err(e) => {
                warn!(
                    "contacts hook: failed to load cache for '{}': {}",
                    account.name, e
                );
                return None;
            }
        };
        let merged = fold(&mut index, &account.own_address());
        if merged == 0 {
            return None;
        }
        if let Err(e) = save_cache(root, &index) {
            warn!(
                "contacts hook: failed to save cache for '{}': {}",
                account.name, e
            );
            return None;
        }
        debug!(
            "contacts hook: merged {merged} messages into '{}' index after a {what}",
            account.name
        );
        Some(index.contacts.len())
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    use crate::config::AccountConfig;
    use crate::ingest::{ingest_message, IngestInput, IngestOutcome};
    use crate::parse::FetchedEmail;
    use crate::store::{BlobStore, Store};

    /// An email with these headers and a fixed body.
    pub fn email(
        from: &str,
        to: &str,
        cc: Option<&str>,
        subject: &str,
        date: &str,
        message_id: &str,
    ) -> FetchedEmail {
        FetchedEmail {
            from: from.into(),
            to: to.into(),
            cc: cc.map(Into::into),
            reply_to: None,
            bcc: None,
            subject: subject.into(),
            date: date.into(),
            body_text: "body".into(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(message_id.into()),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        }
    }

    /// Ingest `email` into `account`'s store, under the data directory this
    /// thread resolves.
    pub fn ingest(
        account: &AccountConfig,
        mailbox: &str,
        uid: i64,
        email: &FetchedEmail,
    ) -> IngestOutcome {
        let store = Store::open(crate::config::store_path(&account.name)).unwrap();
        let blobs = BlobStore::new(crate::config::blobs_dir(&account.name));
        ingest_message(
            &store,
            &blobs,
            &IngestInput {
                account: &account.name,
                mailbox,
                uid,
                email,
                raw: None,
            },
        )
        .unwrap()
    }

    /// The PERSO-234 correspondent, in the two messages that were all the
    /// store held of him: his Outlook meeting accept, filed in the archive,
    /// and the user's own forward to him, with the user on cc.
    pub fn meeting_reply_and_own_forward(
        account: &AccountConfig,
    ) -> [(&'static str, FetchedEmail); 2] {
        [
            (
                "archive",
                email(
                    "\"Hochmuth, Markus\" <Markus.Hochmuth@obermeyer-group.com>",
                    "Sylvain Hellin <me@example.com>",
                    None,
                    "Angenommen: LOC Day",
                    "Wed, 14 Oct 2026 07:59:05 +0000",
                    "<accept-loc-day@obermeyer-group.com>",
                ),
            ),
            (
                "sent",
                email(
                    &account.default_from,
                    "Markus.Hochmuth@obermeyer-group.com",
                    Some(&account.default_from),
                    "FW: LOC Round Table",
                    "Tue, 20 Oct 2026 07:43:50 +0000",
                    "<fw-round-table@example.com>",
                ),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{email, ingest, meeting_reply_and_own_forward};
    use super::*;
    use crate::config::{MailboxMapping, MailboxesConfig};
    use crate::contacts::{build_index_for_account, cache_path, search, Contact};
    use crate::types::{EmailFrontmatter, EmailStatus};
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;

    /// Test fixture: point the data dir at a tempdir for this thread only
    /// (#0077), and create the account directory.
    struct DataDirFixture {
        pub account_root: PathBuf,
        pub _tmp: crate::config::test_env::TestDataDir,
    }

    fn fixture(account_name: &str) -> DataDirFixture {
        let tmp = crate::config::test_env::TestDataDir::new();
        let account_root = account_dir(account_name);
        std::fs::create_dir_all(&account_root).unwrap();
        DataDirFixture {
            account_root,
            _tmp: tmp,
        }
    }

    fn mk_account() -> AccountConfig {
        AccountConfig {
            name: "testaccount".into(),
            default_from: "me@example.com".into(),
            mailboxes: MailboxesConfig {
                inbox: Some(MailboxMapping {
                    server: "INBOX".into(),
                }),
                sent: Some(MailboxMapping {
                    server: "Sent".into(),
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Seed the fixture account's store with one received message, then build
    /// and persist the index from it, exactly as `mp contacts rebuild` does.
    /// The hooks merge into a real rebuilt cache, not into an empty one.
    fn seed_cache_from_store(account: &AccountConfig, root: &Path, from: &str) {
        let seed = email(
            from,
            &account.default_from,
            None,
            "Seed",
            "Fri, 02 Jan 2026 09:00:00 +0000",
            "<seed@example.com>",
        );
        ingest(account, "inbox", 1, &seed);
        let index = build_index_for_account(account).unwrap();
        save_cache(root, &index).unwrap();
    }

    fn mk_draft(to: &str, cc: Option<&str>) -> EmailDraft {
        let to_opt = if to.is_empty() {
            None
        } else {
            Some(to.to_string())
        };
        EmailDraft {
            path: PathBuf::from("/tmp/fake.md"),
            frontmatter: EmailFrontmatter {
                id: None,
                date: None,
                to: to_opt,
                cc: cc.map(|s| s.into()),
                bcc: None,
                subject: "Test".into(),
                status: EmailStatus::Draft,
                from: Some("me@example.com".into()),
                reply_to: None,
                attachments: None,
                sent_at: None,
                sent_via: None,
                message_id: None,
                in_reply_to: None,
                forwarded_from: None,
                signature: None,
                event: None,
                server_message_id: None,
                conflict_of: None,
            },
            body_markdown: String::new(),
        }
    }

    /// Every contact by address, with what a rebuild and a hook must agree on.
    fn by_address(contacts: &HashMap<String, Contact>) -> BTreeMap<String, String> {
        contacts
            .iter()
            .map(|(address, c)| {
                let fact = format!(
                    "{:?} {:?} {} {}",
                    c.tier_tuple(),
                    c.display_name,
                    c.first_seen,
                    c.last_seen
                );
                (address.clone(), fact)
            })
            .collect()
    }

    #[test]
    fn bump_after_send_is_noop_without_cache() {
        let f = fixture("testaccount");
        let account = mk_account();
        let draft = mk_draft("alice@example.com", None);
        // No cache file exists yet.
        bump_after_send(&account, &draft);
        // No cache should have been created.
        assert!(!cache_path(&f.account_root).exists());
    }

    #[test]
    fn bump_after_send_bumps_sent_to_counter() {
        let f = fixture("testaccount");
        let account = mk_account();

        seed_cache_from_store(&account, &f.account_root, "Zoe <zoe@example.com>");
        assert!(cache_path(&f.account_root).exists());
        // The rebuild really read the store, so the seed contact is there.
        let seeded = load_cache(&f.account_root).unwrap().unwrap();
        assert_eq!(seeded.contacts.get("zoe@example.com").unwrap().received, 1);

        let mut draft = mk_draft("Alice <alice@example.com>", Some("bob@example.com"));
        draft.frontmatter.bcc = Some("carol@example.com".into());
        assert_eq!(bump_after_send(&account, &draft), Some(4));

        let loaded = load_cache(&f.account_root).unwrap().unwrap();
        let alice = loaded.contacts.get("alice@example.com").expect("alice");
        assert_eq!(alice.sent_to, 1);
        assert_eq!(alice.display_name, "Alice");
        let bob = loaded.contacts.get("bob@example.com").expect("bob");
        assert_eq!(bob.sent_cc, 1);
        assert_eq!(loaded.contacts["carol@example.com"].sent_cc, 1, "bcc");
    }

    #[test]
    fn bump_after_sync_merges_fresh_observations() {
        let f = fixture("testaccount");
        let account = mk_account();
        seed_cache_from_store(&account, &f.account_root, "Zoe <zoe@example.com>");

        let observations = vec![
            FreshObservation {
                row_id: 2,
                role: "inbox".into(),
                from: "Carol <carol@example.com>".into(),
                to: "me@example.com".into(),
                cc: None,
                bcc: None,
                date: "Mon, 01 Jan 2026 12:00:00 +0000".into(),
            },
            FreshObservation {
                row_id: 3,
                role: "sent".into(),
                from: "me@example.com".into(),
                to: "Dave <dave@example.com>".into(),
                cc: Some("Erin <erin@example.com>".into()),
                bcc: None,
                date: "Tue, 02 Jan 2026 12:00:00 +0000".into(),
            },
        ];
        bump_after_sync(&account, &observations);

        let loaded = load_cache(&f.account_root).unwrap().unwrap();
        assert_eq!(
            loaded.contacts.get("carol@example.com").unwrap().received,
            1
        );
        assert_eq!(loaded.contacts.get("dave@example.com").unwrap().sent_to, 1);
        assert_eq!(loaded.contacts.get("erin@example.com").unwrap().sent_cc, 1);
        // Self-address must be filtered.
        assert!(!loaded.contacts.contains_key("me@example.com"));
    }

    /// PERSO-234: the sync hook folds a message in exactly as the rebuild
    /// folds its row, so an index kept up incrementally is the index a rebuild
    /// of the same store produces. The account's `default_from` carries a
    /// display name, which the hook once compared verbatim and so indexed the
    /// user as their own contact.
    #[test]
    fn the_sync_hook_indexes_what_a_rebuild_indexes() {
        let f = fixture("testaccount");
        let mut account = mk_account();
        account.default_from = "Sylvain Hellin <me@example.com>".into();
        seed_cache_from_store(&account, &f.account_root, "Zoe <zoe@example.com>");

        let mut observations = Vec::new();
        for (uid, (mailbox, message)) in (10..).zip(meeting_reply_and_own_forward(&account)) {
            let outcome = ingest(&account, mailbox, uid, &message);
            assert!(outcome.inserted);
            observations.push(FreshObservation::of(
                outcome.row_id,
                &MailboxRole::from(mailbox),
                &message,
            ));
        }
        bump_after_sync(&account, &observations).expect("the hook wrote the cache");

        let incremental = load_cache(&f.account_root).unwrap().unwrap();
        let hits = search(&incremental, "hochmuth", 20);
        assert_eq!(hits.len(), 1, "searchable without a rebuild");
        let markus = hits[0].contact;
        assert_eq!(markus.address, "markus.hochmuth@obermeyer-group.com");
        assert_eq!(markus.tier_tuple(), (1, 0, 1));
        assert!(!incremental.contacts.contains_key("me@example.com"));

        let rebuilt = build_index_for_account(&account).unwrap();
        assert_eq!(
            by_address(&incremental.contacts),
            by_address(&rebuilt.contacts)
        );
    }

    /// A cache another extractor version built takes no bump: the first read
    /// rebuilds it from the store, which already holds the message.
    #[test]
    fn a_stale_cache_takes_no_bump() {
        let f = fixture("testaccount");
        let account = mk_account();
        seed_cache_from_store(&account, &f.account_root, "Zoe <zoe@example.com>");
        let mut stale = load_cache(&f.account_root).unwrap().unwrap();
        stale.extractor_version = 0;
        save_cache(&f.account_root, &stale).unwrap();

        let draft = mk_draft("Alice <alice@example.com>", None);
        assert_eq!(bump_after_send(&account, &draft), None);
        let kept = load_cache(&f.account_root).unwrap().unwrap();
        assert!(!kept.contacts.contains_key("alice@example.com"));
        assert_eq!(kept.extractor_version, 0, "left for the read to rebuild");
    }

    /// PERSO-234 review: merges that overlap lose nothing. Each one loads the
    /// cache, folds one message in and writes it back; without one lock across
    /// the three, two of them read the same file and the second write drops
    /// the first's message.
    #[test]
    fn overlapping_merges_lose_no_message() {
        let f = fixture("testaccount");
        let account = mk_account();
        seed_cache_from_store(&account, &f.account_root, "Zoe <zoe@example.com>");
        const PER_THREAD: i64 = 40;
        std::thread::scope(|scope| {
            for thread in 0..4i64 {
                let (root, account) = (&f.account_root, &account);
                scope.spawn(move || {
                    for n in 0..PER_THREAD {
                        let observation = FreshObservation {
                            row_id: 100 + thread * PER_THREAD + n,
                            role: "inbox".into(),
                            from: "Carol <carol@example.com>".into(),
                            to: "me@example.com".into(),
                            cc: None,
                            bcc: None,
                            date: "Mon, 01 Jan 2026 12:00:00 +0000".into(),
                        };
                        bump_after_sync_at(root, account, &[observation]);
                    }
                });
            }
        });
        let loaded = load_cache(&f.account_root).unwrap().unwrap();
        assert_eq!(loaded.contacts["carol@example.com"].received, 4 * 40);
    }
}
