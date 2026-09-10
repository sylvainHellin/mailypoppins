//! `mailbox.list`: one account's sidebar hierarchy, with the counts the TUI
//! sidebar shows (P3a-U8).
//!
//! The hierarchy is [`seeds_from_config`]'s, the same one the bootstrap
//! snapshot's `mailboxes` map carries, so the daemon cannot disagree with
//! itself about which mailboxes an account has, in which order, under which
//! role, slug and label. The Drafts mailbox is listed here and excluded from
//! `message.list`, exactly as the sidebar lists it and the message listing
//! refuses it.
//!
//! The three counts, and where each comes from:
//!
//! - `total` is the grouped store query's, one open and one
//!   [`read::mailbox_read_counts`] for the whole account, with the Drafts row
//!   counted from the draft index instead: drafts are not `messages` rows, so a
//!   grouped query cannot see them.
//! - `unread` is the rows of that mailbox without `\Seen`, from the same
//!   grouped query. Drafts have no read state and report `0`.
//! - `badge` is what the sidebar prints beside the label, which is `total`
//!   today. It is a separate member because the snapshot's mailbox view already
//!   carries all three and the two shapes may not diverge.
//!
//! A [`MethodKind::Query`], refusing an unknown account with `-32005` and a
//! storeless one with `-32006`, which is what `message.list` does with the same
//! [`ready_account`](super::account::ready_account) gate.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::AccountConfig;
use crate::daemon::state::seeds_from_config;
use crate::store::read;
use crate::store::Store;
use crate::tui::app::draft_count;

use super::super::dispatch::{
    CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::{internal, string_param};

/// `mailbox.list` as the dispatcher serves it.
pub struct MailboxList {
    /// The live configuration, so a reload is visible to the next listing.
    pub config: Arc<crate::daemon::config::ConfigStore>,
}

impl Method for MailboxList {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new("mailbox.list", MethodKind::Query, 1)
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            list(&params, &self.config.accounts())
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `mailbox.list`.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;

    // The hierarchy `seeds_from_config` derives from `build_mailboxes`, which
    // is the sidebar's own; the counts are looked up by slug, and a mailbox the
    // store has no rows for keeps its slot at zero.
    let seeds = seeds_from_config(std::slice::from_ref(account))
        .into_iter()
        .next()
        .map(|seed| seed.mailboxes)
        .unwrap_or_default();
    let counts = counts_by_mailbox(&name)?;

    let mailboxes: Vec<Value> = seeds
        .iter()
        .map(|seed| {
            // Drafts are local files and not `messages` rows, so the grouped
            // query cannot see them: their total is the draft index's, the same
            // one the TUI sidebar prints, and they have no read state.
            let (total, unread) = if seed.slug == crate::selector::DRAFTS_MAILBOX {
                (draft_count(&name) as u64, 0)
            } else {
                let grouped = counts.get(&seed.slug).copied().unwrap_or_default();
                (grouped.total as u64, grouped.unread as u64)
            };
            json!({
                "role": seed.role,
                "slug": seed.slug,
                "label": seed.label,
                "total": total,
                "unread": unread,
                "badge": total,
            })
        })
        .collect();
    Ok(json!({"account": name, "mailboxes": mailboxes}))
}

/// Both counts of every mailbox of one account, from one open and one grouped
/// query.
///
/// The listing itself is never materialised: counting the unread rows by
/// listing the whole account is the same answer at the cost of every envelope
/// in it, and `SUM` over the flag column is the same predicate
/// [`read::MessageRow::is_read`] applies row by row.
fn counts_by_mailbox(account: &str) -> Result<HashMap<String, read::MailboxReadCounts>, RpcError> {
    let path = crate::config::store_path(account);
    let store = Store::open(&path)
        .map_err(|e| internal(format!("opening the store of {account}: {e:#}")))?;
    read::mailbox_read_counts(&store, account)
        .map_err(|e| internal(format!("counting the mailboxes of {account}: {e:#}")))
}
