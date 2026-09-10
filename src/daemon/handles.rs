//! Materialised handles: the files the daemon writes for a client to open, and
//! the pin they put on the blobs behind them (P3b-U12, `ANO-6`).
//!
//! A client that wants to open an attachment or a browser rendition cannot read
//! the account's blob store itself, so the daemon writes the bytes into its own
//! runtime directory and hands back a path. That file has to outlive the call,
//! and the blobs behind it have to outlive the retention sweep that runs beside
//! it: `store::sweep::sweep_pinned` takes [`HandleTable::pinned_blobs`] and
//! drops every pinned hash from the eviction plan, so a sweep never pulls a file
//! out from under a viewer.
//!
//! **Expiry, not disconnection, ends a handle.** A GUI that opens a contract in
//! a PDF viewer and then loses its socket must keep the file; a crashed client's
//! scratch must not stay on disk forever. So every handle carries an
//! `expires_at` of `now + ttl`, [`DEFAULT_HANDLE_TTL`] is ten minutes, and
//! [`HANDLE_TTL_ENV`] overrides it in milliseconds for tests.
//!
//! **The table takes its clock as a parameter and never reads the wall clock.**
//! `pinned_blobs(now)` ignores an expired handle whether or not [`reap`] has
//! run, so a sweep is never one late tick away from evicting a live blob, and a
//! test drives expiry by arithmetic rather than by sleeping.
//!
//! **Reaping is lazy, not periodic.** [`reap`] runs at the top of every
//! `message.materialise_*` and `message.release_handle` call: it drops the dead
//! entries and unlinks their directories. A periodic tick would buy nothing -
//! the pin is already false the instant a handle expires - and would cost a task
//! whose interval is one more thing to get wrong. The cost is that a daemon
//! nobody calls keeps expired scratch on disk until the next call; the files are
//! inside the 0700 runtime directory, and the next materialisation clears them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use log::warn;

/// How long a materialised handle lives without being released: long enough to
/// look at what was opened, short enough that a crashed client's scratch is gone
/// within one coffee.
pub const DEFAULT_HANDLE_TTL: Duration = Duration::from_secs(600);

/// Overrides [`DEFAULT_HANDLE_TTL`], in milliseconds. Unset, unparseable or
/// zero means the default: a daemon may not fail to start over a stray variable.
pub const HANDLE_TTL_ENV: &str = "MAILYPOPPINS_DAEMON_HANDLE_TTL_MS";

/// The runtime subdirectory holding one directory per live handle:
/// `<data_dir>/runtime/handles/<handle>/<name>`.
pub const HANDLES_DIR: &str = "handles";

/// An opaque handle id, which is also the name of the directory its file lives
/// in: non-empty, and drawn from `[A-Za-z0-9_-]` so `release_handle` can
/// reconstruct the directory from the id alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HandleId(pub String);

impl HandleId {
    /// The id as it travels on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What was materialised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// One attachment of a message, under the name the sender gave it.
    Attachment,
    /// The browser rendition of a message, as `message.html`.
    Html,
}

impl HandleKind {
    /// The word the store uses for this kind of blob.
    pub fn as_str(self) -> &'static str {
        match self {
            HandleKind::Attachment => "attachment",
            HandleKind::Html => "html",
        }
    }
}

/// One live materialisation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handle {
    pub id: HandleId,
    pub kind: HandleKind,
    /// The account whose store the bytes came out of.
    pub account: String,
    /// The file a client opens.
    pub path: PathBuf,
    /// The length of the file at `path`, which is not the size of the backing
    /// blob: a rendition grows by its CSP and charset tags.
    pub bytes: u64,
    /// Every blob this materialisation read, and therefore pins.
    pub blobs: BTreeSet<String>,
    /// The instant the handle dies. Live is strictly before it: the boundary
    /// belongs to the dead.
    pub expires_at: DateTime<Utc>,
}

/// Every live handle of one daemon.
///
/// `&self` everywhere: a [`Method`](super::dispatch::Method) is called through
/// `&self` behind an `Arc` shared by every connection task, so the table carries
/// its own interior mutability rather than forcing the dispatcher to hold a lock
/// it cannot see into.
#[derive(Debug)]
pub struct HandleTable {
    ttl: Duration,
    /// Keyed by id, so [`HandleTable::expire`] returns its ids sorted without
    /// sorting anything.
    live: Mutex<BTreeMap<HandleId, Handle>>,
}

/// Take a lock whose holder may have panicked; the map behind it is whole
/// either way.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl HandleTable {
    /// An empty table whose handles live `ttl`.
    pub fn new(ttl: Duration) -> HandleTable {
        HandleTable {
            ttl,
            live: Mutex::new(BTreeMap::new()),
        }
    }

    /// The same, with [`HANDLE_TTL_ENV`] honoured.
    pub fn from_env() -> HandleTable {
        let millis = std::env::var(HANDLE_TTL_ENV)
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|millis| *millis > 0);
        HandleTable::new(millis.map_or(DEFAULT_HANDLE_TTL, Duration::from_millis))
    }

    /// How long a handle this table mints lives.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Record a materialisation made at `now`, under a fresh id.
    pub fn materialise(
        &self,
        kind: HandleKind,
        account: &str,
        path: PathBuf,
        bytes: u64,
        blobs: &[String],
        now: DateTime<Utc>,
    ) -> Handle {
        self.materialise_with_id(self.mint_id(), kind, account, path, bytes, blobs, now)
    }

    /// An id no live handle holds, for a caller that must name the directory
    /// before it can write the file into it.
    ///
    /// Checked against the table under its lock, so the only way two callers
    /// collide is a 128-bit random draw repeating between this call and the
    /// [`HandleTable::materialise_with_id`] that follows it.
    pub fn mint_id(&self) -> HandleId {
        let live = lock(&self.live);
        loop {
            let id = HandleId(format!("{:032x}", rand::random::<u128>()));
            if !live.contains_key(&id) {
                return id;
            }
        }
    }

    /// [`HandleTable::materialise`] under an id the caller already minted.
    #[allow(clippy::too_many_arguments)]
    pub fn materialise_with_id(
        &self,
        id: HandleId,
        kind: HandleKind,
        account: &str,
        path: PathBuf,
        bytes: u64,
        blobs: &[String],
        now: DateTime<Utc>,
    ) -> Handle {
        let handle = Handle {
            id: id.clone(),
            kind,
            account: account.to_string(),
            path,
            bytes,
            blobs: blobs.iter().cloned().collect(),
            expires_at: now + chrono::Duration::from_std(self.ttl).unwrap_or(chrono::Duration::MAX),
        };
        lock(&self.live).insert(id, handle.clone());
        handle
    }

    /// Drop one handle, answering whether it was there to drop.
    ///
    /// True exactly once per handle: an unknown id, a released id and an id
    /// [`reap`] has already collected are all false, which is what makes
    /// `-32602` the wire answer to all three without the daemon having to tell
    /// them apart. It reads no clock, because an expired entry is dropped by the
    /// reap every call makes before it gets here.
    pub fn release(&self, id: &HandleId) -> bool {
        lock(&self.live).remove(id).is_some()
    }

    /// Every blob a handle that is live at `now` still needs.
    ///
    /// Expired entries are ignored whether or not [`HandleTable::expire`] has
    /// run: a sweep that depended on a reaper would keep a dead handle's blob
    /// alive for as long as the reaper was late.
    pub fn pinned_blobs(&self, now: DateTime<Utc>) -> BTreeSet<String> {
        lock(&self.live)
            .values()
            .filter(|handle| handle.expires_at > now)
            .flat_map(|handle| handle.blobs.iter().cloned())
            .collect()
    }

    /// Delete every handle dead at `now`, returning their ids in order so the
    /// caller can unlink their directories.
    pub fn expire(&self, now: DateTime<Utc>) -> Vec<HandleId> {
        let mut live = lock(&self.live);
        let dead: Vec<HandleId> = live
            .iter()
            .filter(|(_, handle)| handle.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &dead {
            live.remove(id);
        }
        dead
    }
}

/// `<data_dir>/runtime/handles/<handle>`, the directory one handle owns.
///
/// One directory per handle is what lets the file keep the sender's own name
/// without two handles colliding, and what makes a release a directory removal
/// derivable from the id alone.
pub fn handle_dir(id: &HandleId) -> PathBuf {
    super::runtime::runtime_dir()
        .join(HANDLES_DIR)
        .join(id.as_str())
}

/// Remove one handle's directory, tolerating a directory that is already gone.
pub fn remove_handle_dir(id: &HandleId) {
    let dir = handle_dir(id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("[daemon] could not remove {}: {e}", dir.display()),
    }
}

/// Drop every handle dead at `now` and unlink what they left on disk.
///
/// The lazy reaper: called at the top of every handle method, which is the only
/// moment a stale directory costs anything.
pub fn reap(table: &HandleTable, now: DateTime<Utc>) {
    for id in table.expire(now) {
        remove_handle_dir(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lazy reaper unlinks the directory of a handle whose lifetime ran
    /// out, and leaves a live one alone.
    #[test]
    fn reaping_unlinks_what_it_drops() {
        let table = HandleTable::new(Duration::from_secs(60));
        let now = Utc::now();
        let dead = table.materialise(
            HandleKind::Html,
            "alpha",
            PathBuf::from("/nonexistent/message.html"),
            0,
            &[],
            now - chrono::Duration::seconds(120),
        );
        let live = table.materialise(
            HandleKind::Html,
            "alpha",
            PathBuf::from("/nonexistent/message.html"),
            0,
            &[],
            now,
        );

        reap(&table, now);
        assert!(!table.release(&dead.id), "the expired one was collected");
        assert!(table.release(&live.id), "the live one was not");
    }

    /// A zero or negative override is the default rather than a table whose
    /// handles are born dead.
    #[test]
    fn a_zero_lifetime_override_is_the_default() {
        std::env::set_var(HANDLE_TTL_ENV, "0");
        assert_eq!(HandleTable::from_env().ttl(), DEFAULT_HANDLE_TTL);
        std::env::remove_var(HANDLE_TTL_ENV);
    }
}
