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
}

/// Resolve a received-mail query to exactly one row, plus its canonical
/// selector.
///
/// One indexed lookup on `messages_message_id`. Several rows is the normal
/// cross-mailbox copy case, so it is reported with every candidate spelled out
/// in full rather than resolved by a rule the user cannot see.
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
    if let Some(mailbox) = query.mailbox.as_deref() {
        rows.retain(|row| row.mailbox == mailbox);
    }
    match rows.len() {
        0 => Err(not_found(query)),
        1 => {
            let row = rows.remove(0);
            let selector = Selector::for_message(&query.account, &row);
            Ok((row, selector))
        }
        _ => Err(ambiguous(
            query,
            rows.iter()
                .map(|row| Selector::for_message(&query.account, row))
                .collect(),
        )),
    }
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
