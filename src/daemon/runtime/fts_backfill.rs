//! The per-account trigram backfill (PERSO-276): index the messages an older
//! build stored into the trigram index, in the background, until the search
//! can match substrings over all of them.
//!
//! A store an older build wrote gains the trigram index empty on its first
//! open by this build ([`crate::store::fts`]). Every write from then on fills
//! it, and this task fills it with the rows written before: one batch of
//! [`crate::store::fts::backfill_trigram`] at a time on a blocking thread,
//! [`BATCH_PAUSE`] apart, until the batch that stamps the index complete. The
//! search keeps to the word index until then, so nothing waits on it: the
//! daemon starts, serves and syncs while it runs, and a restart resumes from
//! the cursor the last batch persisted. A store already complete costs one
//! store open and one `meta` lookup, and the task ends.
//!
//! # Why a task of its own, beside the runtime
//!
//! Not at the store's open: every `message.search`, the hook runner and the
//! CLI's reads open the store, and none of them should pay minutes of
//! indexing on a large store. Not at the tail of a tick, as the inline
//! classification of PERSO-165 runs: a tick comes once per sync interval
//! (fifteen minutes by default), so a store of tens of thousands of messages
//! would take days to converge. And not under the runtime's turn: a batch is
//! its own short write transaction, which SQLite serialises with an ingest's,
//! so it needs no turn, and taking one would hold every tick and drain behind
//! the backfill or the backfill behind a long sync.
//!
//! A batch holds the write lock for a fraction of a second
//! ([`crate::store::fts::BACKFILL_BYTES`]), and the pause between batches is
//! longer than the longest sleep of SQLite's busy handler, so an ingest
//! waiting on the lock gets it between two batches rather than being starved
//! until its busy timeout.
//!
//! # What gets a backfill
//!
//! A ready runtime: the engine lock holder is the one writer of the store. A
//! blocked runtime's store is the other engine's to fill.
//!
//! # Lifetime
//!
//! The scheduler's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop, and ended by the batch that completes the index.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use log::{debug, info, warn};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::AccountConfig;
use crate::store::blobs::BlobStore;
use crate::store::fts::{backfill_trigram, Backfill};
use crate::store::Store;

use super::account::{AccountRuntime, Readiness};

/// The pause between two batches. SQLite's busy handler sleeps at most
/// 100 ms between two tries, so a writer waiting on the lock retries at least
/// once while the backfill is not holding it.
pub const BATCH_PAUSE: Duration = Duration::from_millis(250);

/// How long a batch that failed waits before the next try.
pub const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Start the backfill for `runtime`, if it gets one (see the module docs).
pub fn spawn(runtime: &Arc<AccountRuntime>, cfg: &AccountConfig) -> Option<JoinHandle<()>> {
    if runtime.readiness() != Readiness::Ready {
        return None;
    }
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        cfg.name.clone(),
        crate::config::store_path(&cfg.name),
        crate::config::blobs_dir(&cfg.name),
        BATCH_PAUSE,
    )))
}

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// One batch, on a blocking thread: it opens SQLite and reads blobs.
async fn batch(store_path: PathBuf, blobs_root: PathBuf) -> anyhow::Result<Backfill> {
    tokio::task::spawn_blocking(move || {
        let store = Store::open(&store_path)?;
        backfill_trigram(&store, &BlobStore::new(blobs_root))
    })
    .await
    .map_err(|e| anyhow::anyhow!("the backfill task {e}"))?
}

/// One account's backfill, until its index is complete or the runtime it was
/// spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    account: String,
    store_path: PathBuf,
    blobs_root: PathBuf,
    pause: Duration,
) {
    let mut indexed = 0usize;
    loop {
        if runtime.upgrade().is_none() || *retired.borrow() {
            debug!("[search] {account}'s runtime retired; stopped the substring index backfill");
            return;
        }
        let wait = match batch(store_path.clone(), blobs_root.clone()).await {
            Ok(done) if done.complete => {
                indexed += done.indexed;
                if indexed > 0 {
                    info!(
                        "[search] {account}: indexed {indexed} stored message(s) for substring \
                         search; the search now matches any part of a word"
                    );
                }
                return;
            }
            Ok(done) => {
                if indexed == 0 {
                    info!(
                        "[search] {account}: indexing the stored messages for substring search \
                         in the background; the search matches whole words until it is done"
                    );
                }
                indexed += done.indexed;
                pause
            }
            Err(e) => {
                warn!(
                    "[search] {account}: the substring index backfill failed, retrying in {}s: \
                     {e:#}",
                    RETRY_AFTER.as_secs()
                );
                RETRY_AFTER
            }
        };
        tokio::select! {
            biased;
            () = stopped(&mut retired) => {
                debug!("[search] {account}'s runtime retired; stopped the substring index backfill");
                return;
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::future::FutureExt;

    use super::super::account::{DrainHook, TickHooks};
    use crate::ingest::{ingest_message, IngestInput};
    use crate::store::fts::{trigram_ready, META_TRIGRAM_COMPLETE, TRIGRAM_TABLE};

    fn quiet_hooks() -> TickHooks {
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(|_ctx| async { Ok(()) }.boxed()),
        }
    }

    /// A store in `dir` holding `rows` messages the way an older build left
    /// it: no trigram index, no completion stamp. The next open creates the
    /// index empty.
    fn older_store(dir: &std::path::Path, rows: i64) {
        let store = Store::open(dir.join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.join("blobs"));
        for uid in 1..=rows {
            let email = crate::parse::FetchedEmail {
                from: "a@example.com".into(),
                to: "me@example.com".into(),
                cc: None,
                reply_to: None,
                bcc: None,
                subject: format!("Dachsanierung {uid}"),
                date: "Thu, 7 Aug 2026 10:00:00 +0000".into(),
                body_text: "Angebot".into(),
                html_body: None,
                has_attachments: false,
                server_has_attachments: None,
                message_id: Some(format!("<m{uid}@example.com>")),
                attachments: Vec::new(),
                flags: Default::default(),
                calendar_ics: None,
                event: None,
            };
            ingest_message(
                &store,
                &blobs,
                &IngestInput {
                    account: "tum",
                    mailbox: "inbox",
                    uid,
                    email: &email,
                    raw: None,
                },
            )
            .unwrap();
        }
        store
            .conn()
            .execute_batch(&format!(
                "DROP TABLE {TRIGRAM_TABLE};
                 DELETE FROM meta WHERE key = '{META_TRIGRAM_COMPLETE}';"
            ))
            .unwrap();
    }

    fn config() -> AccountConfig {
        AccountConfig {
            name: "tum".to_string(),
            ..Default::default()
        }
    }

    /// The backfill runs to completion beside a ready runtime, batch after
    /// batch, and ends; the store then answers substrings.
    #[tokio::test]
    async fn the_backfill_completes_the_index_and_ends() {
        let dir = tempfile::tempdir().unwrap();
        // More rows than one batch takes, so it resumes from its cursor.
        older_store(dir.path(), crate::store::fts::BACKFILL_ROWS as i64 + 5);
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, quiet_hooks()).unwrap(),
        );
        let store_path = dir.path().join("store.sqlite3");
        assert!(!trigram_ready(Store::open(&store_path).unwrap().conn()).unwrap());

        let task = tokio::spawn(run(
            Arc::downgrade(&runtime),
            runtime.retired(),
            "tum".to_string(),
            store_path.clone(),
            dir.path().join("blobs"),
            Duration::from_millis(1),
        ));
        tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .expect("the backfill ended")
            .unwrap();

        let store = Store::open(&store_path).unwrap();
        assert!(trigram_ready(store.conn()).unwrap());
        assert_eq!(crate::store::search::index_drift(&store).unwrap(), (0, 0));
        let hits = crate::store::search::search(
            &store,
            "tum",
            "sanierung",
            None,
            1000,
            crate::store::search::SearchOrder::Newest,
        )
        .unwrap();
        assert_eq!(hits.len(), crate::store::fts::BACKFILL_ROWS + 5);
    }

    /// A retired runtime stops the backfill between two batches, and the
    /// index stays incomplete for the next runtime to resume.
    #[tokio::test]
    async fn the_backfill_stops_when_its_runtime_retires() {
        let dir = tempfile::tempdir().unwrap();
        older_store(dir.path(), crate::store::fts::BACKFILL_ROWS as i64 + 5);
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, quiet_hooks()).unwrap(),
        );
        assert!(runtime.retire(Duration::from_secs(1)).await);
        let task = tokio::spawn(run(
            Arc::downgrade(&runtime),
            runtime.retired(),
            "tum".to_string(),
            dir.path().join("store.sqlite3"),
            dir.path().join("blobs"),
            Duration::from_secs(60),
        ));
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the backfill stopped")
            .unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        assert!(!trigram_ready(store.conn()).unwrap());
    }
}
