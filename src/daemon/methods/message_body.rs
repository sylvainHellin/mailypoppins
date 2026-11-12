//! The body `message.get` answers for a row whose body blob is gone
//! (PERSO-205).
//!
//! The retention sweep evicts body blobs and keeps every row, on the promise
//! that the store is a cache and an evicted body comes back when its message
//! is opened. It comes back here, in the daemon, so `mp show`, `mp read`, the
//! TUI preview and the desktop reader all get it through the one
//! `message.get`, from the first source that has it:
//!
//! 1. the blobs the sweep never evicts: the raw message of an IMAP row, the
//!    HTML of a Graph row ([`read::rederive_body`]), with no network at all;
//! 2. the server: that one message by its `Message-ID`, through the recent
//!    copies the other server reads share ([`ServerCopies`]).
//!
//! A body that hashes to the row's own `body_blob` is stored again
//! ([`crate::ingest::restore_body`]), so the next open reads it from the store,
//! and it counts against the cap like any blob. Only the opened message is
//! fetched, never its mailbox, so a store held at its cap does not download
//! again everything it evicted, the churn #0060 rejected a sync-side refill for.
//!
//! When neither source answers (offline, a server error, a message the server
//! no longer has) the body stays `null` and `body_unavailable` carries the
//! sentence a reader shows in its place.
//!
//! [`ServerCopies`]: super::super::server_copies::ServerCopies

use log::{info, warn};

use crate::config::AccountConfig;
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};

use super::message_eml::ServerLeg;

/// The body of `row`, whose body blob is gone, or the sentence a reader
/// shows when it could not be had.
pub(super) async fn recover(
    account: &AccountConfig,
    row: &MessageRow,
    server: ServerLeg<'_>,
) -> Result<String, String> {
    let name = account.name.as_str();
    // The store is read and closed before the await, as every server leg does.
    let local = {
        let store = open(name)?;
        let blobs = BlobStore::new(crate::config::blobs_dir(name));
        read::rederive_body(&store, &blobs, row.id)
    };
    let body = match local {
        Some(body) => body,
        None => {
            let raw = server
                .download(account, &row.message_id, &row.mailbox)
                .await
                .map_err(|e| unavailable(&e.message))?;
            crate::parse::parse_rfc822_to_fetched_email(&raw)
                .map(|email| email.body_text)
                .ok_or_else(|| unavailable("the server's copy does not parse as a message"))?
        }
    };
    keep(name, row, &body);
    Ok(body)
}

/// Store `body` again for `row` when it is the one the row records; a
/// failure costs the next open another derivation and is only logged.
fn keep(name: &str, row: &MessageRow, body: &str) {
    let kept = Store::open(crate::config::store_path(name)).and_then(|store| {
        let blobs = BlobStore::new(crate::config::blobs_dir(name));
        crate::ingest::restore_body(&store, &blobs, row.id, body)
    });
    match kept {
        Ok(true) => info!(
            "[store] restored the evicted body of {name}:{}/{}",
            row.mailbox, row.uid
        ),
        Ok(false) => info!(
            "[store] the body found again for {name}:{}/{} is not the one stored; shown, not kept",
            row.mailbox, row.uid
        ),
        Err(e) => warn!(
            "[store] restoring the evicted body of {name}:{}/{}: {e:#}",
            row.mailbox, row.uid
        ),
    }
}

fn open(name: &str) -> Result<Store, String> {
    Store::open(crate::config::store_path(name))
        .map_err(|e| unavailable(&format!("opening the store of {name}: {e:#}")))
}

/// The sentence a reader shows in place of a body it could not get back.
fn unavailable(reason: &str) -> String {
    format!(
        "The body of this message is not available locally, and the server did not \
         send it again: {reason}"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::config::AccountConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::daemon::server_copies::{Fetcher, ServerCopies};
    use crate::ingest::{ingest_message, IngestInput};
    use crate::store::blobs::BlobHash;
    use crate::store::sweep::{sweep, SweepOptions};
    use crate::store::{BlobStore, Store};

    use super::super::message_eml::ServerLeg;

    /// A plain-text message as a server hands it over.
    const RAW: &[u8] = b"From: Alice <alice@example.com>\r\n\
To: me@example.com\r\n\
Subject: Evicted\r\n\
Date: Wed, 12 Nov 2025 09:00:00 +0100\r\n\
Message-ID: <evicted@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
The body the retention sweep evicts.\r\n";

    /// One account's store under a temp data root, holding the message of
    /// [`RAW`] in its inbox with the raw message when `with_raw`, and its body
    /// evicted by the retention sweep as a store over its cap does it: a first
    /// over-cap sweep warns, the second evicts.
    struct Evicted {
        _data: crate::config::test_env::DataDirOverride,
        _root: tempfile::TempDir,
        accounts: Vec<AccountConfig>,
        row_id: i64,
        body_hash: String,
    }

    fn evicted(with_raw: bool) -> Evicted {
        let root = tempfile::tempdir().expect("tempdir");
        let data = crate::config::test_env::DataDirOverride::set(root.path());
        crate::config::create_private_dir_all(crate::config::account_dir("tum")).expect("dir");
        let store = Store::open_account("tum").expect("the store");
        let blobs = BlobStore::new(crate::config::blobs_dir("tum"));
        let mut email = crate::parse::parse_rfc822_to_fetched_email(RAW).expect("parses");
        email.flags.seen = true;
        let outcome = ingest_message(
            &store,
            &blobs,
            &IngestInput {
                account: "tum",
                mailbox: "inbox",
                uid: 7,
                email: &email,
                raw: with_raw.then_some(RAW),
            },
        )
        .expect("ingests");
        let body_hash = crate::store::read::find_by_id(&store, outcome.row_id)
            .expect("reads")
            .expect("the row")
            .body_blob
            .expect("a body blob");

        let policy = crate::config::RetentionPolicy {
            max_disk_bytes: 1,
            ..Default::default()
        };
        let evict = SweepOptions {
            dry_run: false,
            force: true,
        };
        sweep(&store, &blobs, &policy, evict).expect("the first sweep warns");
        sweep(&store, &blobs, &policy, evict).expect("the second sweep evicts");
        let hash = crate::store::blobs::BlobHash::parse(&body_hash).expect("a hash");
        assert!(!blobs.contains(&hash), "the sweep evicted the body");
        Evicted {
            _data: data,
            _root: root,
            accounts: vec![AccountConfig {
                name: "tum".to_string(),
                ..Default::default()
            }],
            row_id: outcome.row_id,
            body_hash,
        }
    }

    impl Evicted {
        /// `message.get` of the row, its body asked for, over `copies`.
        async fn get(&self, copies: &ServerCopies) -> Value {
            let server = ServerLeg {
                secrets: Default::default(),
                copies,
            };
            let params = json!({"account": "tum", "row_id": self.row_id});
            super::super::message::get(&params, &self.accounts, server)
                .await
                .expect("message.get answers")
        }

        /// Whether the body blob is on disk, and its refcount.
        fn stored(&self) -> (bool, i64) {
            let store = Store::open_account("tum").expect("the store");
            let blobs = BlobStore::new(crate::config::blobs_dir("tum"));
            let hash = BlobHash::parse(&self.body_hash).expect("a hash");
            (
                blobs.contains(&hash),
                crate::store::blobs::refcount(store.conn(), &hash).expect("refcount"),
            )
        }
    }

    /// A fetcher that fails as an offline server does, counting its calls.
    fn offline() -> (Fetcher, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetcher: Fetcher = Arc::new(move |_request| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(super::super::internal(
                    "connecting to imap.example.com:993: network is unreachable",
                ))
            })
        });
        (fetcher, calls)
    }

    const BODY: &str = "The body the retention sweep evicts.\r\n";

    /// Opening a message whose body the sweep evicted shows its body: the
    /// row's raw message is never evicted, so the body is read back from it
    /// with no server at all, and stored again under the row's own hash with
    /// its reference, so the next open reads the store. The row's flags are
    /// left as they were.
    #[tokio::test]
    async fn an_evicted_body_is_back_when_the_message_is_opened() {
        let fx = evicted(true);
        let (fetcher, downloads) = offline();
        let copies = ServerCopies::new(fetcher);

        let shown = fx.get(&copies).await;
        assert_eq!(shown["body"], json!(BODY), "{shown}");
        assert!(shown.get("body_unavailable").is_none(), "{shown}");
        assert_eq!(shown["flags"], json!(["read"]), "{shown}");
        assert_eq!(fx.stored(), (true, 1), "the body is stored again");
        assert_eq!(fx.get(&copies).await["body"], json!(BODY));
        assert_eq!(downloads.load(Ordering::SeqCst), 0, "no server leg");
    }

    /// A row with no blob to read its body back from (a Graph message sent as
    /// plain text keeps no HTML) asks the server for that one message, once:
    /// the body is stored again, so the second open downloads nothing.
    #[tokio::test]
    async fn a_body_no_blob_holds_is_fetched_from_the_server_once() {
        let fx = evicted(false);
        let (fetcher, downloads) = crate::daemon::server_copies::tests::counting(RAW);
        let copies = ServerCopies::new(fetcher);

        assert_eq!(fx.get(&copies).await["body"], json!(BODY));
        assert_eq!(fx.stored(), (true, 1), "the body is stored again");
        // A fresh table, so a second download could not be the cached copy.
        let (fetcher, again) = crate::daemon::server_copies::tests::counting(RAW);
        assert_eq!(
            fx.get(&ServerCopies::new(fetcher)).await["body"],
            json!(BODY)
        );
        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        assert_eq!(again.load(Ordering::SeqCst), 0);
    }

    /// Offline, the body stays `null` and the reader is told why in a
    /// sentence of its own, never handed an empty body; nothing is stored.
    #[tokio::test]
    async fn an_unreachable_server_answers_a_sentence_and_no_body() {
        let fx = evicted(false);
        let (fetcher, downloads) = offline();
        let copies = ServerCopies::new(fetcher);

        let shown = fx.get(&copies).await;
        assert!(shown["body"].is_null(), "{shown}");
        let why = shown["body_unavailable"].as_str().expect("a reason");
        assert!(why.contains("not available locally"), "{why}");
        assert!(why.contains("network is unreachable"), "{why}");
        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        assert_eq!(fx.stored(), (false, 0), "nothing is stored");
    }
}
