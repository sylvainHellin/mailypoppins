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

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn state() -> MutexGuard<'static, BTreeSet<String>> {
    static OPENING: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    OPENING
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Mark `account`'s store as being opened by its runtime start until the
/// returned [`Mark`] is dropped or [`end`] is called, whichever comes first.
pub fn begin(account: &str) -> Mark {
    state().insert(account.to_string());
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
        end(&self.0);
    }
}

/// The open of `account`'s store is over, whether it succeeded or not. A
/// no-op for an account that was not marked.
pub fn end(account: &str) {
    state().remove(account);
}

/// Whether `account`'s store is still being opened.
pub fn is_opening(account: &str) -> bool {
    state().contains(account)
}

/// Run `owe` when `account`'s store is still being opened, under the lock
/// [`end`] takes, answering its result; `None` means the store is open and
/// `owe` did not run. A `sender.block` records the Inbox move it owes the
/// account here, so the start, which reads the owed moves after [`end`],
/// finds it.
pub fn while_opening<T>(account: &str, owe: impl FnOnce() -> T) -> Option<T> {
    let state = state();
    state.contains(account).then(owe)
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
        drop(mark);
        assert!(!is_opening(account));
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
