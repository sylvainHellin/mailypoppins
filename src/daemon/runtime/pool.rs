//! The per-account read pool (P3b-U4, sized by P1a-U5).
//!
//! `docs/baselines/decisions/read-pool.md` measured the question this module
//! answers: how many read connections one account's runtime holds so that a
//! preview read is not stuck behind a 5000-row listing. The answer is
//! [`DEFAULT_READ_POOL_SIZE`], two, and the writer connection is not one of
//! them: a sync transaction must never occupy a read slot.
//!
//! ## Why a pool of whole connections and not one shared connection
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, which
//! `docs/plans/preview-latency.md` records twice. A connection cannot be parked
//! behind a shared reference and used from several tasks; it can only be
//! *moved* to whoever is reading right now and moved back afterwards, which is
//! why a checkout is a guard ([`PooledRead`]) rather than a borrow.
//!
//! The measurement's own prototype pinned each connection to a dedicated thread
//! and dispatched jobs to it over a channel. This one hands the connection to
//! the calling thread instead, because the signature the plan fixes,
//! `read_conn(&self) -> PooledRead`, gives the caller a connection rather than
//! taking its query. The property the numbers depend on is the same either way:
//! it is the bound of two concurrent readers per account that keeps a preview
//! off the back of a long read, not where the connection lives.
//!
//! ## Why exhaustion is a wait and not an error
//!
//! [`ReadPool::get`] blocks on a condition variable until a connection comes
//! back; an error would push a retry loop into every caller for a condition
//! that resolves in microseconds. [`ReadPool::try_get`] is the non-blocking
//! form, for a caller that needs to know rather than wait.
//!
//! Every connection is opened through [`Store::open`], so it carries the
//! store's pragmas - WAL, `busy_timeout`, `synchronous = NORMAL`,
//! `foreign_keys = ON` - and a pool opened over a path that has no store yet
//! creates one, which is how a runtime's first read also brings the account's
//! store into existence.

use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use anyhow::{Context, Result};

use crate::store::Store;

/// Read connections per account runtime, the size `docs/baselines/decisions/read-pool.md`
/// chose. The writer connection is separate and outside the pool.
pub const DEFAULT_READ_POOL_SIZE: usize = 2;

/// A bounded set of read connections over one account's store.
pub struct ReadPool {
    inner: Arc<PoolInner>,
}

/// The shared half, held by the pool and by every checked-out connection so a
/// [`PooledRead`] can hand its connection back after the pool value itself has
/// been dropped.
struct PoolInner {
    /// How many connections exist, checked out or not. Fixed at open time.
    size: usize,
    /// The connections nobody is using. Never longer than `size`.
    idle: Mutex<Vec<Store>>,
    /// Signalled whenever a connection returns to `idle`.
    free: Condvar,
}

/// Take a lock whose holder may have panicked. A poisoned pool is still a
/// correct pool: the data behind the mutex is a list of connections, and a
/// panic between `pop` and `push` cannot leave one half-moved.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ReadPool {
    /// Open `size` connections over the store at `store_path`, clamped to at
    /// least one: a pool of zero connections could never serve a read.
    ///
    /// The connections are opened serially, so the first one creates the store
    /// (and validates it) before the rest attach to it.
    pub fn open(store_path: &Path, size: usize) -> Result<Self> {
        let size = size.max(1);
        let mut idle = Vec::with_capacity(size);
        for _ in 0..size {
            idle.push(Store::open(store_path).with_context(|| {
                format!(
                    "opening a pooled read connection on {}",
                    store_path.display()
                )
            })?);
        }
        Ok(ReadPool {
            inner: Arc::new(PoolInner {
                size,
                idle: Mutex::new(idle),
                free: Condvar::new(),
            }),
        })
    }

    /// How many connections this pool owns.
    pub fn size(&self) -> usize {
        self.inner.size
    }

    /// Check a connection out, waiting until one is free.
    ///
    /// Blocking, so a caller on an async runtime reaches it through
    /// `spawn_blocking` exactly as it reaches any other store read.
    pub fn get(&self) -> PooledRead {
        let mut idle = lock(&self.inner.idle);
        loop {
            if let Some(store) = idle.pop() {
                drop(idle);
                return PooledRead {
                    store: Some(store),
                    pool: Arc::clone(&self.inner),
                };
            }
            idle = self
                .inner
                .free
                .wait(idle)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Check a connection out if one is free right now, `None` if every
    /// connection is out.
    pub fn try_get(&self) -> Option<PooledRead> {
        let store = lock(&self.inner.idle).pop()?;
        Some(PooledRead {
            store: Some(store),
            pool: Arc::clone(&self.inner),
        })
    }
}

impl std::fmt::Debug for ReadPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadPool")
            .field("size", &self.inner.size)
            .field("idle", &lock(&self.inner.idle).len())
            .finish()
    }
}

/// One checked-out read connection, returned to its pool on drop.
///
/// Derefs to the [`rusqlite::Connection`] underneath, so a caller runs its own
/// SQL as it does through [`Store::conn`].
pub struct PooledRead {
    /// `None` only between [`Drop`] taking the store out and the value dying.
    store: Option<Store>,
    pool: Arc<PoolInner>,
}

impl Deref for PooledRead {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        self.store
            .as_ref()
            .expect("a checked-out connection is only taken back in Drop")
            .conn()
    }
}

impl Drop for PooledRead {
    fn drop(&mut self) {
        if let Some(store) = self.store.take() {
            lock(&self.pool.idle).push(store);
            // One waiter, because one connection came back.
            self.pool.free.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool is bounded by its size, and a checkout returns on drop.
    #[test]
    fn a_pool_is_bounded_and_refills_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite3");

        let pool = ReadPool::open(&path, 2).expect("a pool opens and creates the store");
        assert_eq!(pool.size(), 2);
        assert!(
            path.exists(),
            "the first pooled connection created the store"
        );

        let first = pool.try_get().expect("one");
        let second = pool.try_get().expect("two");
        assert!(pool.try_get().is_none(), "a pool of two hands out two");
        drop(second);
        assert!(pool.try_get().is_some(), "a dropped checkout comes back");
        drop(first);
    }

    /// A pooled connection carries the store's pragmas, `busy_timeout`
    /// included: it is a [`Store::open`] connection and not a bare
    /// `Connection::open`.
    #[test]
    fn a_pooled_connection_carries_the_stores_pragmas() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite3");
        let pool = ReadPool::open(&path, 1).expect("a pool opens");
        let read = pool.get();

        let journal: String = read
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal_mode is readable");
        assert!(journal.eq_ignore_ascii_case("wal"), "got {journal}");

        let busy: i64 = read
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy_timeout is readable");
        assert!(busy > 0, "a pooled connection waits on a busy writer");

        let foreign_keys: i64 = read
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("foreign_keys is readable");
        assert_eq!(foreign_keys, 1);
    }

    /// Zero is not a pool: it is clamped to one connection.
    #[test]
    fn a_size_below_one_is_clamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = ReadPool::open(&dir.path().join("store.sqlite3"), 0).expect("a pool opens");
        assert_eq!(pool.size(), 1);
        assert!(pool.try_get().is_some());
    }
}
