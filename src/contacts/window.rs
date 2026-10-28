//! Which sync observations a rebuild of the contacts index has already
//! counted, decided by when they were made rather than by row id (PERSO-234).
//!
//! A rebuild walks the store without the cache lock, and syncs keep inserting
//! and merging meanwhile, so its commit has to settle two things: a row
//! inserted after the walk whose merge landed in the cache the commit is
//! about to replace must not be lost, and a row the walk counted whose merge
//! arrives after the commit must not be counted twice.
//!
//! Row ids cannot settle either. `messages.id` is an `INTEGER PRIMARY KEY`
//! without `AUTOINCREMENT`, so deleting the newest row hands its id to the
//! next insert: a watermark of "the highest id the walk saw" skipped the next
//! new correspondent's message for good, and a catch-up of "the rows above
//! it" missed the same message.
//!
//! Every [`FreshObservation`] instead carries a sequence number drawn from one
//! process-wide counter right after its row was inserted
//! ([`next_sequence`]), and a rebuild opens a [`RebuildWindow`] before its
//! walk lists the store, which records the counter at that moment. Ingest
//! commits each message in a transaction of its own before it returns, so an
//! observation numbered below the mark is of a row committed before the
//! listing began, which the walk saw; one numbered at or above it may not
//! have been seen.
//!
//! - While a window is open, every sync merge logs the observations it was
//!   handed at or above the mark, whether or not it could merge them into the
//!   cache on disk, and the commit folds the log into the rebuilt index.
//! - Once the rebuild's index is written, its mark becomes the account's
//!   `counted_below`: a merge drops every observation numbered below it,
//!   which the walk counted.
//!
//! The one race left counts a row twice and loses none: a row committed
//! before the listing whose observation is numbered at or above the mark.
//! That is a row whose insert lands between the window opening and the
//! walk's listing statement, the few milliseconds the walk takes to open the
//! store, or whose ingest returned just before the window opened and was
//! numbered just after. Losing a correspondent is the bug this guards
//! against; a counter one too high in that instant is not.
//!
//! The state is this process's alone, which is enough: the daemon is the
//! cache's only writer, and an observation never outlives the process that
//! made it. Rebuilds of one account are serialised (the window waits for the
//! one before it), so there is at most one window per account at a time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

use crate::sync::FreshObservation;

/// The process-wide counter every observation is numbered from. Starts at 1,
/// so a hand-built observation numbered 0 predates every window.
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// The next observation number, drawn by [`FreshObservation::of`] the moment
/// the row it observes has been inserted.
pub fn next_sequence() -> u64 {
    SEQUENCE.fetch_add(1, Ordering::SeqCst)
}

/// One observation a merge logged while a window was open, and whether the
/// merge got it into the cache on disk.
#[derive(Debug)]
pub struct Logged {
    pub observation: FreshObservation,
    pub applied: bool,
}

/// An account's window state.
#[derive(Default)]
struct Account {
    /// Observations numbered below this were counted by the walk of the
    /// rebuild whose index is on disk.
    counted_below: u64,
    /// The open window's mark and log.
    open: Option<(u64, Vec<Logged>)>,
}

struct State {
    accounts: Mutex<HashMap<PathBuf, Account>>,
    /// Signalled when a window closes, for a rebuild waiting to open one.
    closed: Condvar,
}

fn state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State {
        accounts: Mutex::new(HashMap::new()),
        closed: Condvar::new(),
    })
}

fn accounts() -> MutexGuard<'static, HashMap<PathBuf, Account>> {
    state()
        .accounts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A rebuild's claim on its account, from before its walk lists the store to
/// its commit. Dropping it without committing (a cancelled or failed rebuild)
/// closes the window and leaves the account's state as it was.
#[derive(Debug)]
pub struct RebuildWindow {
    root: PathBuf,
    mark: u64,
    closed: bool,
}

impl RebuildWindow {
    /// Open the window of the account directory `root`, waiting for one
    /// already open there to close. Call it before the walk's listing.
    pub fn open(root: &Path) -> RebuildWindow {
        let root = root.to_path_buf();
        let mut accounts = accounts();
        while accounts.get(&root).is_some_and(|a| a.open.is_some()) {
            accounts = state()
                .closed
                .wait(accounts)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let mark = SEQUENCE.load(Ordering::SeqCst);
        accounts.entry(root.clone()).or_default().open = Some((mark, Vec::new()));
        RebuildWindow {
            root,
            mark,
            closed: false,
        }
    }

    /// Close the window for the commit, answering what the merges logged
    /// meanwhile. The caller holds the cache lock, and calls [`committed`]
    /// once the rebuilt index is written.
    pub fn close(mut self) -> (u64, Vec<Logged>) {
        self.closed = true;
        let logged = take_open(&self.root).unwrap_or_default();
        (self.mark, logged)
    }
}

impl Drop for RebuildWindow {
    fn drop(&mut self) {
        if !self.closed {
            take_open(&self.root);
        }
    }
}

fn take_open(root: &Path) -> Option<Vec<Logged>> {
    let logged = accounts()
        .get_mut(root)
        .and_then(|a| a.open.take())
        .map(|(_, logged)| logged);
    state().closed.notify_all();
    logged
}

/// The rebuild whose window was marked `mark` wrote its index: from now on
/// an observation numbered below `mark` is one its walk counted.
pub fn committed(root: &Path, mark: u64) {
    accounts()
        .entry(root.to_path_buf())
        .or_default()
        .counted_below = mark;
}

/// The observations a merge still owes the cache under `root`: those the
/// rebuild on disk did not count. Called under the cache lock.
pub fn uncounted<'a>(
    root: &Path,
    observations: &'a [FreshObservation],
) -> Vec<&'a FreshObservation> {
    let counted_below = accounts().get(root).map_or(0, |a| a.counted_below);
    observations
        .iter()
        .filter(|obs| obs.sequence >= counted_below)
        .collect()
}

/// Log a merge's observations into the account's open window, if there is
/// one, keeping those the window's walk may not have seen. Called under the
/// cache lock, after the merge, with whether it reached the disk.
pub fn log(root: &Path, observations: &[&FreshObservation], applied: bool) {
    let mut accounts = accounts();
    let Some((mark, logged)) = accounts.get_mut(root).and_then(|a| a.open.as_mut()) else {
        return;
    };
    let mark = *mark;
    logged.extend(
        observations
            .iter()
            .filter(|obs| obs.sequence >= mark)
            .map(|obs| Logged {
                observation: (*obs).clone(),
                applied,
            }),
    );
}
