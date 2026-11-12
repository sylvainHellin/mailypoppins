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

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn set() -> MutexGuard<'static, BTreeSet<String>> {
    static OPENING: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    OPENING
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Mark `account`'s store as being opened by its runtime start until the
/// returned [`Mark`] is dropped or [`end`] is called, whichever comes first.
pub fn begin(account: &str) -> Mark {
    set().insert(account.to_string());
    Mark(account.to_string())
}

/// The owner of one [`begin`]: dropping it ends the mark, so a start task
/// that panicked does not leave its account refused as `opening` for good.
#[must_use = "dropping the mark ends it at once"]
#[derive(Debug)]
pub struct Mark(String);

impl Drop for Mark {
    fn drop(&mut self) {
        end(&self.0);
    }
}

/// The open of `account`'s store is over, whether it succeeded or not; a
/// no-op for an account that was not marked.
pub fn end(account: &str) {
    set().remove(account);
}

/// Whether `account`'s store is still being opened.
pub fn is_opening(account: &str) -> bool {
    set().contains(account)
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
}
