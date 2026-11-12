//! The accounts whose store this daemon is still opening at startup
//! (PERSO-109).
//!
//! The first open of a store file in a process runs `PRAGMA integrity_check`,
//! a full walk of the file that took 12 to 16 s on a 1.6 GB store. The daemon
//! serves from the moment it is bound and does that open once per account, in
//! the account's runtime start, off the accept loop. Until it has come back,
//! a method that needs the store answers `-32006` `account_not_ready` with
//! `state: "opening"` ([`crate::daemon::methods::account::ready_account`])
//! rather than opening the file itself: a second open would run a second
//! integrity check of the same file at the same time.
//!
//! The set is filled before the daemon serves, one name per configured
//! account whose runtime it is about to start, so no call can reach a store in
//! the window before its start has begun; [`end`] is called by the start just
//! before it commits the account's state, whatever that state is. Process-wide
//! because the integrity check it guards is per process, and keyed by account
//! name because one daemon serves one data directory.
//!
//! A `sender.block` cannot move an opening account's Inbox mail to Junk
//! without its store, and the blocked-senders runner only judges mail that
//! arrives later, so the move is owed to the account, in a file the start
//! reads after [`end`] ([`crate::daemon::senders::owe`]). The debt is
//! written under the lock [`end`] takes ([`while_opening`]), so it is either
//! on disk before the start reads the file or the store is open and the
//! block moves the mail itself.
//!
//! The refusal ends before the start commits, since the start reads the store
//! after [`end`] to complete the account's seed and run the owed move, but
//! `daemon.status` keeps reporting the account `opening` until the start
//! calls [`committed`] just before it commits ([`is_starting`]). The runtime
//! table holds the account's runtime from before [`end`], and reading that
//! alone, the status said `ready` while a store method still answered
//! `-32006`, to a client that had waited for no account to be opening.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The accounts whose startup start is in flight, in its two stages.
#[derive(Default)]
struct Marks {
    /// Refused by the store methods: from [`begin`] to [`end`].
    opening: BTreeSet<String>,
    /// Reported `opening` by `daemon.status`: from [`begin`] to
    /// [`committed`].
    starting: BTreeSet<String>,
}

fn state() -> MutexGuard<'static, Marks> {
    static MARKS: OnceLock<Mutex<Marks>> = OnceLock::new();
    MARKS
        .get_or_init(|| Mutex::new(Marks::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Mark `account`'s store as being opened by its runtime start until the
/// returned [`Mark`] is dropped or [`end`] is called, whichever comes first,
/// and the account as starting until the mark is dropped or [`committed`] is
/// called.
pub fn begin(account: &str) -> Mark {
    let mut marks = state();
    marks.opening.insert(account.to_string());
    marks.starting.insert(account.to_string());
    Mark(account.to_string())
}

/// The owner of one [`begin`]: dropping it ends the mark, so a start task
/// that panicked does not leave its account refused as `opening` for good.
#[must_use = "dropping the mark ends it at once"]
#[derive(Debug)]
pub struct Mark(String);

impl Drop for Mark {
    fn drop(&mut self) {
        // A mark dropped before its start ended it belongs to an account
        // that left the configuration or a start that panicked: an owed
        // block move stays in its file for the account's next start.
        committed(&self.0);
    }
}

/// The open of `account`'s store is over, whether it succeeded or not. A
/// no-op for an account that was not marked.
pub fn end(account: &str) {
    state().opening.remove(account);
}

/// The start of `account` is about to commit the account's state: the store
/// is open, and its seed and owed move are done. Ends the refusal too, for a
/// caller that did not [`end`] it. A no-op for an account that was not
/// marked.
pub fn committed(account: &str) {
    let mut marks = state();
    marks.opening.remove(account);
    marks.starting.remove(account);
}

/// Whether `account`'s store is still being opened.
pub fn is_opening(account: &str) -> bool {
    state().opening.contains(account)
}

/// Whether `account`'s startup start has not committed its state yet, which
/// `daemon.status` reports as `opening` whatever the runtime table holds.
pub fn is_starting(account: &str) -> bool {
    state().starting.contains(account)
}

/// Test-only hook on [`crate::store::FAKE_OPEN_HOLD_ENV`]'s directory: the
/// start of `account`'s runtime waits, once the runtime is filed and before
/// the start commits the account's state, for as long as
/// `<dir>/<account>.commit.hold` exists, after recording
/// `<dir>/<account>.commit.held`.
///
/// The open hold stops a start before its runtime exists; this one stops it
/// in the window after, which a test store's start crosses in microseconds,
/// so a test can ask what `daemon.status` and a store method answer there.
/// Released after [`crate::store::FAKE_OPEN_HOLD_LIMIT`] whatever the file
/// says, as the open hold is.
pub async fn hold_commit(account: &str) {
    let Some(dir) = std::env::var_os(crate::store::FAKE_OPEN_HOLD_ENV).filter(|d| !d.is_empty())
    else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let gate = dir.join(format!("{account}.commit.hold"));
    if !gate.exists() {
        return;
    }
    if let Err(e) = std::fs::write(dir.join(format!("{account}.commit.held")), b"") {
        log::warn!("[daemon] commit hold marker for {account}: {e}");
    }
    let started = std::time::Instant::now();
    while gate.exists() && started.elapsed() < crate::store::FAKE_OPEN_HOLD_LIMIT {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Run `owe` when `account`'s store is still being opened, under the lock
/// [`end`] takes, answering its result; `None` means the store is open and
/// `owe` did not run. A `sender.block` records the Inbox move it owes the
/// account here, so the start, which reads the owed moves after [`end`],
/// finds it.
pub fn while_opening<T>(account: &str, owe: impl FnOnce() -> T) -> Option<T> {
    let marks = state();
    marks.opening.contains(account).then(owe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_is_opening_between_begin_and_end() {
        let account = "opening-unit-test";
        assert!(!is_opening(account));
        let mark = begin(account);
        assert!(is_opening(account));
        end(account);
        assert!(!is_opening(account));
        drop(mark);
        assert!(!is_opening(account), "a second end is a no-op");
    }

    #[test]
    fn dropping_the_mark_ends_it() {
        let account = "opening-unit-test-drop";
        let mark = begin(account);
        assert!(is_opening(account));
        assert!(is_starting(account));
        drop(mark);
        assert!(!is_opening(account));
        assert!(!is_starting(account));
    }

    #[test]
    fn an_account_is_starting_until_its_start_commits() {
        let account = "opening-unit-test-commit";
        let mark = begin(account);
        end(account);
        assert!(!is_opening(account), "the store methods serve it");
        assert!(is_starting(account), "the status still reads opening");
        committed(account);
        assert!(!is_starting(account));
        drop(mark);
        assert!(!is_starting(account), "a second commit is a no-op");
    }

    #[test]
    fn a_block_move_is_owed_only_while_the_account_opens() {
        let account = "opening-unit-test-owed";
        assert_eq!(while_opening(account, || 1), None);
        let mark = begin(account);
        assert_eq!(while_opening(account, || 2), Some(2));
        end(account);
        assert_eq!(while_opening(account, || 3), None);
        drop(mark);
    }
}
