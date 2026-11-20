//! The store half of the `mp://` selector contract (#0126, P5-U10a).
//!
//! The grammar, the parser, the formatter and the percent-encoding are
//! [`mp_core::selector`] and are re-exported here unchanged, so
//! `crate::selector::parse`, `crate::selector::Selector` and the rest resolve
//! exactly as they did when this file held all of it. What stayed is the pair
//! of resolvers, which is the only part that reads an index: one lookup in
//! `messages`, one in `drafts`.

use anyhow::Result;

pub use mp_core::selector::*;

use crate::store::drafts::{self, DraftRow};
use crate::store::read::{self, MessageRow};
use crate::store::Store;

/// The store's received-mail row, read as a selector's source. The canonical
/// selector of a row is a formatting decision and lives in `mp-core`; the row
/// is a store type and lives here.
impl MessageRowRef for MessageRow {
    fn mailbox(&self) -> &str {
        &self.mailbox
    }

    fn message_id(&self) -> &str {
        &self.message_id
    }

    fn uid(&self) -> i64 {
        self.uid
    }
}

/// Resolve a received-mail query to exactly one row, plus its canonical
/// selector.
///
/// One indexed lookup on `messages_message_id`. Several rows is the normal
/// cross-mailbox copy case, so it is reported with every candidate spelled out
/// in full rather than resolved by a rule the user cannot see; two rows in one
/// mailbox (two server copies) are spelled with their UID, which a selector
/// that ends in one narrows to (PERSO-239).
///
/// The selector answered names the copy resolved: a row whose mailbox holds a
/// twin keeps its UID, as a listing prints it, so a client that resolves first
/// and addresses the row again by the answered selector (`mp export`, `mp
/// save`) reaches the same copy instead of an ambiguity.
pub fn resolve_received(store: &Store, query: &SelectorQuery) -> Result<(MessageRow, Selector)> {
    debug_assert_eq!(query.namespace, Namespace::Received);
    // Ingest stores the header verbatim, brackets and all, while the selector
    // key is the bare identifier; so the bracketed form is asked first and the
    // bare one second, which also answers for a row whose stored id has no
    // brackets. Both are the same indexed lookup on `messages_message_id`.
    let key = message_key(&query.key);
    let mut rows = read::find_by_message_id(store, &query.account, &format!("<{key}>"))?;
    if rows.is_empty() {
        rows = read::find_by_message_id(store, &query.account, key)?;
    }
    // Every copy of the Message-ID is among the rows, so their twins are the
    // mailbox's, asked before any filter narrows them away.
    let twins = Twins::among(rows.iter());
    if let Some(mailbox) = query.mailbox.as_deref() {
        rows.retain(|row| row.mailbox == mailbox);
    }
    if let Some(uid) = query.uid {
        rows.retain(|row| row.uid == uid);
    }
    match rows.len() {
        0 => Err(not_found(query)),
        1 => {
            let row = rows.remove(0);
            let selector = Selector::for_listed(&query.account, &row, &twins);
            Ok((row, selector))
        }
        _ => Err(ambiguous(
            query,
            rows.iter()
                .map(|row| (Selector::for_message(&query.account, row), row.uid))
                .collect(),
        )),
    }
}

/// The selector a client addresses one stored row by: the canonical one,
/// narrowed to the row's UID when its mailbox holds a twin (PERSO-244).
///
/// What every single-row answer carries (`message.get`, `message.attachments`,
/// a mutation's), so the selector a client is handed back resolves to the
/// copy it was handed for.
pub fn of_row(store: &Store, account: &str, row: &MessageRow) -> Result<Selector> {
    let twins = read::twins_of(store, account, std::iter::once(row))?;
    Ok(Selector::for_listed(account, row, &twins))
}

/// Resolve a draft query to exactly one indexed draft, plus its canonical
/// selector. The drafts table is keyed `(account, id)`, so there is no
/// ambiguous case here: a duplicate id is impossible by the primary key.
pub fn resolve_draft(store: &Store, query: &SelectorQuery) -> Result<(DraftRow, Selector)> {
    debug_assert_eq!(query.namespace, Namespace::Drafts);
    match drafts::find(store, &query.account, &query.key)? {
        Some(row) => {
            let selector = Selector::for_draft(&query.account, &row.id);
            Ok((row, selector))
        }
        None => Err(not_found(query)),
    }
}
