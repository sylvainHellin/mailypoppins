//! The server copies of messages this daemon downloaded in the last couple of
//! minutes, so the reads of one search hit share one download (PERSO-155).
//!
//! A server-only search hit has no row, so every action on it reads the
//! server's copy: `message.get` lists its parts, then one
//! `message.materialise_attachment` per part, the read-only view, the `.eml`,
//! a forward. Each used to download the whole message again. The copies are
//! kept here by account, mailbox key and `Message-ID`, which is the address a
//! hit and a row without a raw blob both download by.
//!
//! Bounded both ways: [`CAPACITY`] copies at most, the oldest dropped first,
//! and none older than [`TTL`], which is long enough for a user to go from the
//! attachment list to the part they want and short enough that a message
//! changed on the server is read afresh soon after. A configuration swap that
//! removes or changes an account drops that account's copies
//! ([`ServerCopies::forget_account`]): a copy is the bytes of a server that
//! may no longer be the account's.
//!
//! Concurrent reads of one copy share one download: the entry is a
//! [`tokio::sync::OnceCell`] the first caller fills and the others await. A
//! download that fails leaves the cell empty, so the next read tries again.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use tokio::sync::OnceCell;

use mp_protocol::RpcError;

use crate::config::AccountConfig;
use crate::secrets::SecretsBackendKind;

/// How many copies are kept at most.
///
/// A few: the cache exists for the run of reads on the one hit a user is
/// acting on, not to hold a mailbox, and a copy can be tens of megabytes.
pub const CAPACITY: usize = 4;

/// How long a copy is served after its download started.
pub const TTL: Duration = Duration::from_secs(120);

/// One download, as the daemon asks a server for it.
#[derive(Debug, Clone)]
pub struct Request {
    pub account: AccountConfig,
    pub secrets: SecretsBackendKind,
    pub message_id: String,
    /// The `messages.mailbox` key the copy is looked up in.
    pub mailbox: String,
}

/// What downloads a copy: the server leg in the daemon, a counter in a test.
pub type Fetcher =
    Arc<dyn Fn(Request) -> BoxFuture<'static, Result<Vec<u8>, RpcError>> + Send + Sync>;

/// The key of one copy: account, mailbox key, `Message-ID`.
type Key = (String, String, String);

struct Entry {
    key: Key,
    started: Instant,
    bytes: Arc<OnceCell<Vec<u8>>>,
}

/// The daemon's one table of recent server copies.
pub struct ServerCopies {
    entries: Mutex<VecDeque<Entry>>,
    capacity: usize,
    ttl: Duration,
    fetcher: Fetcher,
}

impl ServerCopies {
    /// The table the daemon serves with, downloading through `fetcher`.
    pub fn new(fetcher: Fetcher) -> Self {
        Self::with_limits(CAPACITY, TTL, fetcher)
    }

    /// A table with its own bounds, for the tests that exercise them.
    pub fn with_limits(capacity: usize, ttl: Duration, fetcher: Fetcher) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
            ttl,
            fetcher,
        }
    }

    /// The RFC 822 bytes of `request`'s message: the copy downloaded in the
    /// last [`TTL`], or a fresh download, which is kept.
    pub async fn get(&self, request: Request) -> Result<Vec<u8>, RpcError> {
        let key = (
            request.account.name.clone(),
            request.mailbox.clone(),
            request.message_id.clone(),
        );
        let cell = self.cell(key);
        let fetcher = Arc::clone(&self.fetcher);
        cell.get_or_try_init(|| fetcher(request)).await.cloned()
    }

    /// Drop every copy of `account`'s, for a swap that removed or changed it.
    pub fn forget_account(&self, account: &str) {
        self.lock().retain(|entry| entry.key.0 != account);
    }

    /// How many copies are held, expired ones included until the next read.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no copy is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The cell of `key`, made when there is none or the one there expired,
    /// with every expired entry dropped and the oldest one past
    /// [`CAPACITY`] evicted.
    fn cell(&self, key: Key) -> Arc<OnceCell<Vec<u8>>> {
        let mut entries = self.lock();
        let ttl = self.ttl;
        entries.retain(|entry| entry.started.elapsed() < ttl);
        if let Some(entry) = entries.iter().find(|entry| entry.key == key) {
            return Arc::clone(&entry.bytes);
        }
        while entries.len() >= self.capacity {
            entries.pop_front();
        }
        let bytes = Arc::new(OnceCell::new());
        entries.push_back(Entry {
            key,
            started: Instant::now(),
            bytes: Arc::clone(&bytes),
        });
        bytes
    }

    /// The entries, whatever a panicking holder left: they are a cache, and
    /// a stale one costs a download, never a wrong answer.
    fn lock(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fetcher that answers `raw` and counts its calls.
    pub(crate) fn counting(raw: &'static [u8]) -> (Fetcher, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetcher: Fetcher = Arc::new(move |_request| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(raw.to_vec()) })
        });
        (fetcher, calls)
    }

    fn request(account: &str, mailbox: &str, message_id: &str) -> Request {
        Request {
            account: AccountConfig {
                name: account.to_string(),
                ..Default::default()
            },
            secrets: SecretsBackendKind::default(),
            message_id: message_id.to_string(),
            mailbox: mailbox.to_string(),
        }
    }

    /// Reads of one copy share one download; another mailbox, message or
    /// account is a download of its own.
    #[tokio::test]
    async fn reads_of_one_copy_share_one_download() {
        let (fetcher, calls) = counting(b"raw");
        let copies = ServerCopies::new(fetcher);
        for _ in 0..3 {
            assert_eq!(
                copies.get(request("a", "sent", "<m@x>")).await.unwrap(),
                b"raw"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        copies.get(request("a", "inbox", "<m@x>")).await.unwrap();
        copies.get(request("a", "sent", "<n@x>")).await.unwrap();
        copies.get(request("b", "sent", "<m@x>")).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    /// Two reads in flight at once wait for the one download.
    #[tokio::test]
    async fn concurrent_reads_wait_for_the_one_download() {
        let (fetcher, calls) = counting(b"raw");
        let copies = ServerCopies::new(fetcher);
        let (one, two) = tokio::join!(
            copies.get(request("a", "sent", "<m@x>")),
            copies.get(request("a", "sent", "<m@x>")),
        );
        assert_eq!(
            (one.unwrap(), two.unwrap()),
            (b"raw".to_vec(), b"raw".to_vec())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A copy past its lifetime is downloaded again.
    #[tokio::test]
    async fn an_expired_copy_is_downloaded_again() {
        let (fetcher, calls) = counting(b"raw");
        let copies = ServerCopies::with_limits(CAPACITY, Duration::ZERO, fetcher);
        copies.get(request("a", "sent", "<m@x>")).await.unwrap();
        copies.get(request("a", "sent", "<m@x>")).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Past the capacity the oldest copy goes, and the newer ones stay.
    #[tokio::test]
    async fn the_oldest_copy_goes_past_the_capacity() {
        let (fetcher, calls) = counting(b"raw");
        let copies = ServerCopies::with_limits(2, TTL, fetcher);
        for id in ["<1@x>", "<2@x>", "<3@x>"] {
            copies.get(request("a", "sent", id)).await.unwrap();
        }
        assert_eq!(copies.len(), 2);
        copies.get(request("a", "sent", "<3@x>")).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3, "<3@x> is still held");
        copies.get(request("a", "sent", "<1@x>")).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4, "<1@x> was evicted");
    }

    /// A failed download is not kept: the next read tries again.
    #[tokio::test]
    async fn a_failed_download_is_tried_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetcher: Fetcher = Arc::new(move |_request| {
            let call = counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if call == 0 {
                    Err(RpcError {
                        code: -32603,
                        message: "offline".to_string(),
                        data: None,
                    })
                } else {
                    Ok(b"raw".to_vec())
                }
            })
        });
        let copies = ServerCopies::new(fetcher);
        assert!(copies.get(request("a", "sent", "<m@x>")).await.is_err());
        assert_eq!(
            copies.get(request("a", "sent", "<m@x>")).await.unwrap(),
            b"raw"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Forgetting an account drops its copies and keeps the others'.
    #[tokio::test]
    async fn forgetting_an_account_drops_only_its_copies() {
        let (fetcher, calls) = counting(b"raw");
        let copies = ServerCopies::new(fetcher);
        copies.get(request("a", "sent", "<m@x>")).await.unwrap();
        copies.get(request("b", "sent", "<m@x>")).await.unwrap();
        copies.forget_account("a");
        assert_eq!(copies.len(), 1);
        copies.get(request("b", "sent", "<m@x>")).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        copies.get(request("a", "sent", "<m@x>")).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
