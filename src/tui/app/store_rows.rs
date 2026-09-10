//! The pre-daemon store-backed readers, kept for exactly two callers (P5-U4,
//! #0124).
//!
//! Until Phase 5 these three were the TUI's read path: `load_emails` filled the
//! list, `count_all_emails` filled the sidebar column, and
//! `App::load_message_body` filled the preview memo. They read the account's
//! store directly, which is what the daemon migration exists to end, so the
//! paint path no longer calls any of them: it goes through
//! [`crate::tui::queries`] and the session the `App` holds.
//!
//! They are still here, and in this file rather than in
//! `src/tui/app/{mod.rs,types.rs}`, for two reasons and no third:
//!
//! - **They are the equality oracle.** `src/tui/app/queries_tests.rs` compares
//!   every daemon-backed answer field for field against the answer these
//!   produce over the same seeded store, in the same process. An oracle that
//!   was deleted with the call sites would leave the query layer pinned by
//!   nothing but itself.
//! - **They are what an `App` with no session reads.** A `Session::connect`
//!   that wedges is not fatal (P5-U2), and, more to the point, every one of the
//!   ~370 TUI unit tests builds an `App` with no session at all.
//!
//! P5-U10 moves `src/tui/` into a crate that may not link the store, which is
//! where this file dies; until then it is dead weight the tests carry, not a
//! path a frame takes.

use crate::store::open_store;
use crate::store::read;

use super::types::{
    draft_count, entry_from_row, load_drafts, mailbox_key, status_for_mailbox, EmailEntry,
    MailboxInfo, MessageRef,
};

/// Load one mailbox of one account from the store, newest first.
///
/// `mailbox` is the role or slug ingest recorded, which is the leaf of the
/// `MailboxInfo::dir` the sidebar carries (see
/// [`mailbox_key`](super::types::mailbox_key)).
///
/// There is no directory walk and no fallback to one. After #0037 nothing
/// writes `.md`, so a message that is not in the store is an ingest bug, and a
/// walk that produced it anyway would hide that bug behind a slow path.
/// A store that cannot be opened or queried logs and yields an empty list,
/// which is what a mailbox that has never synced looks like anyway.
///
/// One SQL query and no blob reads at all: the bodies are loaded lazily, one
/// at a time behind the preview (see `PreviewBody`) and once per list
/// generation behind body search.
pub fn load_emails(account: &str, mailbox: &str) -> Vec<EmailEntry> {
    let mut span =
        crate::timing::TimingSpan::with_context("load_emails", format!("{account}/{mailbox}"));
    if mailbox == crate::selector::DRAFTS_MAILBOX {
        let entries = load_drafts(account);
        span.mark(&format!("{} draft(s) from the index", entries.len()));
        return entries;
    }
    let Some(store) = open_store(account) else {
        return Vec::new();
    };
    let rows = match read::list_mailbox(&store, account, mailbox) {
        Ok(rows) => rows,
        Err(e) => {
            log::warn!("[store] listing {account}/{mailbox} failed: {e:#}");
            return Vec::new();
        }
    };
    span.mark(&format!("{} row(s), no blob reads", rows.len()));

    let status = status_for_mailbox(mailbox);
    rows.into_iter()
        .map(|row| entry_from_row(row, &status))
        .collect()
}

/// Per-mailbox message counts for the sidebar, as one grouped query.
///
/// Index-aligned with `mailboxes`: a mailbox the store has no rows for counts
/// zero, so a configured-but-never-synced mailbox keeps its slot rather than
/// shifting every count after it.
///
/// Drafts are the exception, and have to be: they are not `messages` rows, so
/// the grouped query cannot see them and the sidebar would show 0 next to a
/// populated list. That count comes from the drafts index, the same refresh
/// plus read the Drafts mailbox load itself does.
pub fn count_all_emails(account: &str, mailboxes: &[MailboxInfo]) -> Vec<usize> {
    let store = open_store(account);
    let counts = store
        .as_ref()
        .and_then(|store| match read::mailbox_counts(store, account) {
            Ok(counts) => Some(counts),
            Err(e) => {
                log::warn!("[store] counting mailboxes for {account} failed: {e:#}");
                None
            }
        })
        .unwrap_or_default();
    mailboxes
        .iter()
        .map(|mb| {
            let key = mailbox_key(mb);
            if key == crate::selector::DRAFTS_MAILBOX {
                draft_count(account)
            } else {
                counts.get(&key).copied().unwrap_or(0)
            }
        })
        .collect()
}

/// Read one message body from one account's blob store.
///
/// `None` means the row itself is gone, which is a stale reference rather than
/// an evicted body; the preview shows an empty body either way, and the log
/// says which happened.
pub fn load_message_body(account: &str, msg: MessageRef) -> Option<String> {
    let store = open_store(account)?;
    let blobs = crate::store::BlobStore::for_account(account);
    let body = crate::store::read::load_body(&store, &blobs, msg.row_id());
    if body.is_none() {
        log::warn!("[store] {msg} is not in the store; previewing an empty body");
    }
    body
}
