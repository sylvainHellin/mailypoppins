//! The drafts an armed undo-send hold is waiting to send (#0143), which no
//! pull may change under it.
//!
//! The hold lives in the daemon's [`HoldScheduler`](crate::daemon::hold), and
//! the reconcile runs in an account runtime that holds no handle on it, so the
//! scheduler records here what it arms and takes out what fires or is
//! cancelled. One table per process, as the scheduler is one per daemon.
//!
//! A `send.draft` hold names its draft; a `send.approved` hold names none and
//! holds every approved draft of its account, which is what the batch sends
//! when it fires.

use std::sync::{Mutex, PoisonError};

/// One armed hold: its account, and its draft's `id:`, `None` for a batch.
type Held = (String, Option<String>);

static HELD: Mutex<Vec<Held>> = Mutex::new(Vec::new());

/// Record a hold armed on `account` for `draft_id`, or for every approved
/// draft when `draft_id` is `None`.
pub fn arm(account: &str, draft_id: Option<&str>) {
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((account.to_string(), draft_id.map(str::to_string)));
}

/// Forget one hold [`arm`] recorded: it fired or was cancelled.
pub fn release(account: &str, draft_id: Option<&str>) {
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(at) = held
        .iter()
        .position(|(a, d)| a == account && d.as_deref() == draft_id)
    {
        held.remove(at);
    }
}

/// Whether a hold is waiting to send the draft `draft_id` of `account`, which
/// is `approved` or not.
pub fn is_held(account: &str, draft_id: &str, approved: bool) -> bool {
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|(a, d)| {
            a == account
                && match d {
                    Some(id) => id == draft_id,
                    None => approved,
                }
        })
}
