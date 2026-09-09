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
//! - `total` is [`count_all_emails`]'s, the grouped store query the TUI sidebar
//!   runs, with the Drafts row counted from the draft index instead: drafts are
//!   not `messages` rows, so a grouped query cannot see them.
//! - `unread` is the rows of that mailbox without `\Seen`. Drafts have no read
//!   state and report `0`.
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
use crate::tui::app::{build_mailboxes, count_all_emails};

use super::super::dispatch::{
    CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::{internal, string_param};

/// `mailbox.list` as the dispatcher serves it.
pub struct MailboxList {
    /// The accounts `config.toml` declared when this daemon started.
    pub accounts: Arc<Vec<AccountConfig>>,
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
            list(&params, &self.accounts)
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `mailbox.list`.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;

    // Index-aligned by construction: `seeds_from_config` maps `build_mailboxes`
    // one to one, and `count_all_emails` is documented to keep a slot for a
    // mailbox the store has no rows for.
    let seeds = seeds_from_config(std::slice::from_ref(account))
        .into_iter()
        .next()
        .map(|seed| seed.mailboxes)
        .unwrap_or_default();
    let totals = count_all_emails(&name, &build_mailboxes(account));
    let unread = unread_counts(&name)?;

    let mailboxes: Vec<Value> = seeds
        .iter()
        .enumerate()
        .map(|(index, seed)| {
            let total = totals.get(index).copied().unwrap_or(0) as u64;
            json!({
                "role": seed.role,
                "slug": seed.slug,
                "label": seed.label,
                "total": total,
                "unread": unread.get(&seed.slug).copied().unwrap_or(0),
                "badge": total,
            })
        })
        .collect();
    Ok(json!({"account": name, "mailboxes": mailboxes}))
}

/// How many messages of each mailbox the server has not flagged `\Seen`.
///
/// One pass over the account rather than one query per mailbox, and through the
/// same [`read`] path every other listing takes, so an unread count and a
/// listing cannot disagree about which rows a mailbox holds.
fn unread_counts(account: &str) -> Result<HashMap<String, u64>, RpcError> {
    let path = crate::config::store_path(account);
    let store = Store::open(&path)
        .map_err(|e| internal(format!("opening the store of {account}: {e:#}")))?;
    let rows = read::list_account(&store, account)
        .map_err(|e| internal(format!("listing the messages of {account}: {e:#}")))?;
    let mut counts: HashMap<String, u64> = HashMap::new();
    for row in rows.iter().filter(|row| !row.is_read()) {
        *counts.entry(row.mailbox.clone()).or_default() += 1;
    }
    Ok(counts)
}
