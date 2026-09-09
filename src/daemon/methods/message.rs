//! `message.list`: one mailbox of one account, newest first.
//!
//! The rows are [`crate::store::read::list_mailbox`]'s, so the daemon answers
//! from the same query, in the same order (`date_sort DESC, id DESC`), as
//! `mp list-messages` and the TUI list. `date_sort` is
//! [`crate::tui::app::resolve_date`]'s sort key, so all three stacks derive a
//! date the same way rather than each parsing the header again, and
//! `date_display` is the `Date:` header as the store holds it, which is the
//! column a listing prints.
//!
//! `total` is how many messages the mailbox holds and ignores `limit`, which is
//! the "In the store: N" of `mp list-messages`. `limit: null` and an absent
//! `limit` both mean "all"; `0` means none, since `null` already spells "all"
//! and a number may not mean the opposite of itself.

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::AccountConfig;
use crate::selector::DRAFTS_MAILBOX;
use crate::store::read::{self, MessageRow};
use crate::store::Store;
use crate::tui::app::{build_mailboxes, resolve_date};

use super::super::dispatch::{
    CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::{internal, invalid_params, string_param};

/// `message.list` as the dispatcher serves it.
pub struct MessageList {
    /// The accounts `config.toml` declared when this daemon started.
    pub accounts: Arc<Vec<AccountConfig>>,
}

impl Method for MessageList {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new("message.list", MethodKind::Query, 1)
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        // The store read is synchronous, as it was when the server called this
        // method directly. Moving it onto a blocking thread is a change to how
        // the daemon schedules work, not to how it dispatches, so it belongs
        // with the account runtimes of Phase 5.
        Box::pin(async move {
            list(&params, &self.accounts)
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `message.list`.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let wanted = string_param(params, "mailbox")?;
    let limit = limit_param(params)?;

    let account = super::account::ready_account(accounts, &name)?;
    let mailbox = resolve_mailbox(account, &wanted)?;

    let path = crate::config::store_path(&name);
    let store =
        Store::open(&path).map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let rows = read::list_mailbox(&store, &name, &mailbox)
        .map_err(|e| internal(format!("listing {name}/{mailbox}: {e:#}")))?;

    let total = rows.len();
    let messages: Vec<Value> = rows
        .iter()
        .take(limit.unwrap_or(total))
        .map(to_json)
        .collect();
    Ok(json!({
        "account": name,
        "mailbox": mailbox,
        "total": total,
        "messages": messages,
    }))
}

/// One stored row on the wire.
///
/// The three nullable headers travel as `""` rather than `null`, because the
/// shape says `str`. `flags` carries the three axes the protocol names and not
/// the store's fourth (`\Flagged`): a client that needs the star waits for the
/// version that adds it.
///
/// Both dates are here because neither can be derived from the other:
/// `date_sort` is `resolve_date`'s UTC sort key, and `date_display` is the
/// `Date:` header as the store holds it, which is the column a listing prints.
/// A client renders from the wire alone rather than reading the store beside
/// the daemon.
pub fn to_json(row: &MessageRow) -> Value {
    let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
    let flags = row.flags();
    json!({
        "uid": row.uid,
        "message_id": row.message_id,
        "from": row.from.clone().unwrap_or_default(),
        "subject": row.subject.clone().unwrap_or_default(),
        "date_sort": date_sort,
        "date_display": row.date_display.clone().unwrap_or_default(),
        "flags": {
            "seen": flags.seen,
            "answered": flags.answered,
            "forwarded": flags.forwarded,
        },
        "has_attachments": row.has_attachments,
    })
}

/// `limit`, which is optional and unsigned; anything else is `-32602`.
fn limit_param(params: &Value) -> Result<Option<usize>, RpcError> {
    match params.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|limit| Some(limit.min(usize::MAX as u64) as usize))
            .ok_or_else(|| {
                invalid_params("limit is a non-negative integer, or null for every message")
            }),
    }
}

/// The mailbox id behind what the caller asked for.
///
/// Role, slug or sidebar label, exactly as `mp list-messages --mailbox` accepts
/// them, and the answer echoes the resolved id rather than the spelling. An
/// unknown one is `-32602` naming the mailboxes this account has, because the
/// caller asked for something that does not exist rather than for something the
/// daemon refuses.
fn resolve_mailbox(account: &AccountConfig, wanted: &str) -> Result<String, RpcError> {
    let mailboxes: Vec<_> = build_mailboxes(account)
        .into_iter()
        .filter(|mailbox| mailbox.id != DRAFTS_MAILBOX)
        .collect();
    if let Some(hit) = mailboxes
        .iter()
        .find(|m| wanted.eq_ignore_ascii_case(&m.id) || wanted.eq_ignore_ascii_case(&m.label))
    {
        return Ok(hit.id.clone());
    }
    let known: Vec<&str> = mailboxes.iter().map(|m| m.id.as_str()).collect();
    Err(invalid_params(format!(
        "'{wanted}' is not a mailbox of {} (known: {})",
        account.name,
        known.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> AccountConfig {
        AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        }
    }

    /// A role, a label and an unknown name, which is the error that names the
    /// alternatives.
    #[test]
    fn a_mailbox_resolves_by_role_or_by_label_and_never_to_drafts() {
        assert_eq!(resolve_mailbox(&account(), "inbox").unwrap(), "inbox");
        assert_eq!(resolve_mailbox(&account(), "Inbox").unwrap(), "inbox");
        let refused = resolve_mailbox(&account(), "drafts").expect_err("drafts is not listable");
        assert_eq!(refused.code, -32602);
        assert!(refused.message.contains("inbox"), "{}", refused.message);
    }

    /// `null`, absent, `0` and a number are four different answers.
    #[test]
    fn the_limit_parameter_tells_null_from_zero() {
        assert_eq!(limit_param(&json!({})).unwrap(), None);
        assert_eq!(limit_param(&json!({"limit": null})).unwrap(), None);
        assert_eq!(limit_param(&json!({"limit": 0})).unwrap(), Some(0));
        assert_eq!(limit_param(&json!({"limit": 5})).unwrap(), Some(5));
        assert!(limit_param(&json!({"limit": "5"})).is_err());
        assert!(limit_param(&json!({"limit": -1})).is_err());
    }
}
