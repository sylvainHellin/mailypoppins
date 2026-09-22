//! The sessionless store-backed readers the daemon's answers are compared
//! against (#0124 P5-U4, #0126 P5-U10e).
//!
//! Until Phase 5 these were the TUI's read path: `load_emails` filled the list,
//! `count_all_emails` filled the sidebar column, `load_message_body` filled the
//! preview memo, and three readers on the `App` answered the agenda, the
//! invitation card and the raw iMIP blob. Every one of them opened the
//! account's store, which is what the daemon migration exists to end, and the
//! paint path stopped calling any of them when the query layer landed.
//!
//! They are here, in the root crate's test tree rather than under `src/tui/`,
//! for one reason: **they are the equality oracle**. [`super::queries`],
//! [`super::invites`] and [`super::types`] compare every daemon-backed answer
//! field for field against the answer these produce over the same seeded store,
//! in the same process. An oracle deleted with its call sites would leave the
//! query layer pinned by nothing but itself.
//!
//! What they are no longer is a fallback. An `App` with no session answers an
//! empty list, zeroed counts and no card, which is what P5-U8's "no direct
//! fallback" asked for and what `crates/mp-tui` makes true by construction: a
//! client crate that cannot link the store cannot read one behind the daemon's
//! back. The store reads live on this side of the boundary, in the crate that
//! owns the store, and they are reached by tests only.

use std::path::Path;

use mp_protocol::listing::MessageListRow;

use crate::store::open_store;
use crate::store::read;
use crate::tui::app::{
    entry_from_draft, entry_from_row, entry_from_skip, mailbox_key, resolve_date,
    status_for_mailbox, CalendarEvent, EmailEntry, MailboxInfo, MessageRef,
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
pub(super) fn load_emails(account: &str, mailbox: &str) -> Vec<EmailEntry> {
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
    rows.iter()
        .map(|row| entry_from_row(row_to_wire(account, row), &status))
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
pub(super) fn count_all_emails(account: &str, mailboxes: &[MailboxInfo]) -> Vec<usize> {
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
                crate::draft::draft_count(account)
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
pub(super) fn load_message_body(account: &str, msg: MessageRef) -> Option<String> {
    let store = open_store(account)?;
    let blobs = crate::store::BlobStore::for_account(account);
    let body = crate::store::read::load_body(&store, &blobs, msg.row_id());
    if body.is_none() {
        log::warn!("[store] {msg} is not in the store; previewing an empty body");
    }
    body
}

/// The Drafts mailbox, listed from the drafts index instead of `messages`
/// (#0050 scope item 5).
///
/// Drafts are the one local-only thing in the product: they are `.md` files an
/// agent or `$EDITOR` writes behind the application's back, so there is no
/// `messages` row to list and the index is what the CLI and the TUI share.
/// The refresh is paid here rather than assumed, because a mailbox load is
/// exactly the moment the answer has to be current; the one-second fingerprint
/// poll in the event loop is what notices a change *between* loads.
///
/// Every status the index holds is listed, `sent` included: the lister filters
/// nothing, so a file someone hand-edited to `status: sent` still shows, which
/// is the escape hatch it should be. What no longer shows is a draft this
/// application sent to every recipient and recorded in the outbox, because
/// such a send retires the file (see [`crate::draft::settle_sent_draft`]); a
/// *partial* send, or one with no durable record, keeps it, marked `sent` and
/// addressable.
pub(super) fn load_drafts(account: &str) -> Vec<EmailEntry> {
    let (rows, skipped) = crate::draft::indexed_drafts(account);
    // The unparseable files lead the list: they are the ones the user is
    // hunting for ("my draft disappeared"), and they have no date to sort by,
    // so pinning them to the top is both honest and useful (#0080).
    skipped
        .into_iter()
        .map(entry_from_skip)
        .chain(rows.into_iter().map(entry_from_draft))
        .collect()
}

/// One stored row as `message.list` would have sent it.
///
/// The oracle's half of the listing equality, the same construction
/// `src/daemon/methods/message.rs`'s `to_json` performs over the same row.
/// `mailbox` is not a field of the wire row: a listing answer names the
/// mailbox once, and the selector the daemon rendered carries it.
fn row_to_wire(account: &str, row: &crate::store::read::MessageRow) -> MessageListRow {
    let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
    let flags = row.flags();
    MessageListRow {
        id: row.id,
        uid: row.uid,
        message_id: row.message_id.clone(),
        from: row.from.clone().unwrap_or_default(),
        to: row.to.clone().unwrap_or_default(),
        cc: row.cc.clone(),
        reply_to: row.reply_to.clone(),
        bcc: row.bcc.clone(),
        subject: row.subject.clone().unwrap_or_default(),
        date_sort,
        date_display: row.date_display.clone().unwrap_or_default(),
        flags: mp_protocol::listing::MessageFlags {
            seen: flags.seen,
            answered: flags.answered,
            forwarded: flags.forwarded,
            flagged: flags.flagged,
        },
        has_attachments: row.has_attachments,
        is_invite: row.is_invite,
        selector: crate::selector::Selector::for_message(account, row).to_string(),
    }
}

// ---------------------------------------------------------------------------
// The three readers that were methods on `App`
// ---------------------------------------------------------------------------

/// The agenda of one account, as the Calendar view holds it.
///
/// The wire rows [`crate::agenda`] builds, decoded the way the client decodes
/// the ones that come off the socket: `calendar.events` is the same builder
/// over the same store, which is what makes the method a move rather than a
/// second implementation.
pub(super) fn calendar_events(account: &str, self_address: &str) -> Vec<CalendarEvent> {
    let Some(store) = open_store(account) else {
        return Vec::new();
    };
    let blobs = crate::store::BlobStore::for_account(account);
    crate::agenda::load_events_for_account(&store, &blobs, account, self_address)
        .into_iter()
        .map(CalendarEvent::from_wire)
        .collect()
}

/// One message's ics blob parsed into the event the card renders, with the
/// store's REPLY rows folded in, which is what `message.invite` answers.
pub(super) fn message_invite(
    account: &str,
    msg: MessageRef,
    self_address: &str,
) -> Option<crate::types::EventFrontmatter> {
    let store = open_store(account)?;
    let blobs = crate::store::BlobStore::for_account(account);
    crate::reconcile::event_for_message(&store, &blobs, account, msg.row_id(), self_address)
}

/// The raw `invite.ics` bytes of one message, which is what `message.ics`
/// answers.
pub(super) fn message_ics(account: &str, msg: MessageRef) -> Option<Vec<u8>> {
    let store = open_store(account)?;
    let blobs = crate::store::BlobStore::for_account(account);
    read::load_invite_ics(&store, &blobs, msg.row_id())
}

/// One draft's body, read from the file the drafts index points at, which is
/// what `draft.path` plus a client-side parse answers.
///
/// [`crate::store::Store::open`] rather than [`open_store`], for the reason
/// every drafts path gives: drafts are local-only files, so an account that has
/// never synced has no store *file* and still has drafts. A plain read,
/// deliberately: the index is consumed, not refreshed.
pub(super) fn draft_body(account: &str, id: &str) -> Option<String> {
    let store = crate::store::Store::open(crate::config::store_path(account))
        .map_err(|e| log::warn!("[drafts] could not open the store for {account}: {e:#}"))
        .ok()?;
    let row = match crate::store::drafts::find(&store, account, id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            log::warn!("[drafts] {id} is no longer indexed; previewing an empty body");
            return None;
        }
        Err(e) => {
            log::warn!("[drafts] looking up {id}: {e:#}");
            return None;
        }
    };
    match crate::draft::parse_email_draft(&row.path) {
        Ok(draft) => Some(draft.body_markdown),
        Err(e) => {
            log::warn!("[drafts] reading {}: {e:#}", row.path.display());
            None
        }
    }
}
