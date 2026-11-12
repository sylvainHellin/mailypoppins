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
//! arrives later, so the move is owed to the account ([`owe_block_move`]):
//! [`end`] hands the owed entries to the start, which moves that mail once
//! the store is open. One lock covers the set and the debts, so an entry is
//! either owed before [`end`] takes the debts or moved by the call itself.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The accounts being opened, and the blocked-sender entries whose Inbox move
/// each of them is owed.
#[derive(Default)]
struct Opening {
    accounts: BTreeSet<String>,
    owed: BTreeMap<String, Vec<String>>,
}

fn state() -> MutexGuard<'static, Opening> {
    static OPENING: OnceLock<Mutex<Opening>> = OnceLock::new();
    OPENING
        .get_or_init(|| Mutex::new(Opening::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Mark `account`'s store as being opened by its runtime start until the
/// returned [`Mark`] is dropped or [`end`] is called, whichever comes first.
pub fn begin(account: &str) -> Mark {
    state().accounts.insert(account.to_string());
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
        // that left the configuration or a start that panicked: nothing is
        // left to move the owed mail on.
        end(&self.0);
    }
}

/// The open of `account`'s store is over, whether it succeeded or not: the
/// blocked-sender entries whose Inbox move it is owed ([`owe_block_move`]),
/// in the order they were blocked. A no-op answering nothing for an account
/// that was not marked.
pub fn end(account: &str) -> Vec<String> {
    let mut state = state();
    state.accounts.remove(account);
    state.owed.remove(account).unwrap_or_default()
}

/// Whether `account`'s store is still being opened.
pub fn is_opening(account: &str) -> bool {
    state().accounts.contains(account)
}

/// Owe `account` the move of `entry`'s mail in its Inbox to Junk when its
/// store is still being opened, answering whether it was owed; `false` means
/// the store is open and the caller moves the mail itself.
pub fn owe_block_move(account: &str, entry: &str) -> bool {
    let mut state = state();
    if !state.accounts.contains(account) {
        return false;
    }
    let owed = state.owed.entry(account.to_string()).or_default();
    if !owed.iter().any(|known| known == entry) {
        owed.push(entry.to_string());
    }
    true
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
        assert!(end(account).is_empty());
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
    fn a_block_move_is_owed_only_while_the_account_opens_and_handed_over_once() {
        let account = "opening-unit-test-owed";
        assert!(!owe_block_move(account, "spam@example.com"));
        let mark = begin(account);
        assert!(owe_block_move(account, "spam@example.com"));
        assert!(owe_block_move(account, "@junk.example"));
        assert!(owe_block_move(account, "spam@example.com"));
        assert_eq!(end(account), ["spam@example.com", "@junk.example"]);
        assert!(!owe_block_move(account, "late@example.com"));
        drop(mark);
        assert!(end(account).is_empty(), "the debts were handed over once");
    }
}
