//! The `contact.*` family: the frecency index, read and rebuilt (P4-U14).
//!
//! ```text
//! contact.rebuild  {account}                -> {operation_id}
//!                      settles {account, contacts, kept, saved, cache_path}
//! contact.search   {account, query, limit}  -> {account, query, contacts}
//! contact.stats    {account}                -> {account, total, sent_to, …}
//! ```
//!
//! A row is `{address, display_name, sent_to, sent_cc, received, score}`: the
//! field names of [`crate::contacts::Contact`] plus the match score, because
//! `mp contacts search --parsable` prints `address\tdisplay_name` and a
//! renamed pair would make the tab-delimited line a translation rather than a
//! projection (`ANO-8`).
//!
//! `contact.rebuild` is an [`MethodKind::Operation`] and the two reads are
//! queries. A rebuild walks every row of every mailbox of the account and
//! re-ranks the result; on a real mailbox that is seconds, it is what the
//! TUI's `r` key runs, and a GUI wants to watch it.
//!
//! The two reads take [`super::account::ready_account`], so an account with no
//! store is `-32006`; the rebuild takes
//! [`super::account::configured_account`], so it does *not*, because
//! `build_index_for_account` treats a storeless account as an empty index and
//! `mp contacts rebuild` with no `--account` has always walked every
//! configured account including those.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::AccountConfig;
use crate::contacts::{
    build_index_for_account, cache_path, load_cache, save_rebuilt_cache, search, CacheSave,
    ContactIndex, MatchResult,
};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry, Progress};
use super::super::state::ConnectionId;
use super::{internal, only_params, string_param};

/// The phase a rebuild reports under, which is the family's own word.
pub const REBUILD_PHASE: &str = "contacts";

/// How many rows `contact.stats` carries, which is what `mp contacts stats`
/// prints under `Top 10:`.
const TOP_ROWS: usize = 10;

/// The three methods of the family, in method-name order.
pub const CONTACT_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("contact.rebuild", MethodKind::Operation, 1),
    MethodSpec::new("contact.search", MethodKind::Query, 1),
    MethodSpec::new("contact.stats", MethodKind::Query, 1),
];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
) {
    for spec in CONTACT_METHOD_SPECS {
        dispatcher.register(Arc::new(ContactMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
        }));
    }
}

/// One of the three, selected by its own [`MethodSpec`].
pub struct ContactMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
}

impl Method for ContactMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            match self.spec.name {
                "contact.search" => {
                    only_params("contact.search", &params, &["account", "limit", "query"])?;
                    let name = string_param(&params, "account")?;
                    let account = super::account::ready_account(&snapshot.accounts, &name)?.clone();
                    Ok(Outcome::query(search_result(&account, &params)?))
                }
                "contact.stats" => {
                    only_params("contact.stats", &params, &["account"])?;
                    let name = string_param(&params, "account")?;
                    let account = super::account::ready_account(&snapshot.accounts, &name)?.clone();
                    Ok(Outcome::query(stats_result(&account)?))
                }
                _ => {
                    only_params("contact.rebuild", &params, &["account"])?;
                    let name = string_param(&params, "account")?;
                    let account =
                        super::account::configured_account(&snapshot.accounts, &name)?.clone();
                    let (id, handle) = self.operations.start(
                        ConnectionId(ctx.connection_id),
                        self.spec.cancel_scope,
                        self.spec.name,
                    );
                    tokio::spawn(rebuild(account, handle));
                    Ok(Outcome::query(json!({"operation_id": id.as_str()})))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// The reads
// ---------------------------------------------------------------------------

/// `contact.search`: the ranked rows a query matches, capped at `limit`.
///
/// A query nothing matches is an empty array rather than a refusal: "no
/// matches" is an answer.
fn search_result(account: &AccountConfig, params: &Value) -> Result<Value, RpcError> {
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(20);
    let index = load_index(account)?;
    Ok(json!({
        "account": account.name,
        "query": query,
        "contacts": rows(&search(&index, &query, limit)),
    }))
}

/// `contact.stats`: the totals, the cache the answer was read from, when it
/// was built, and the top rows.
fn stats_result(account: &AccountConfig) -> Result<Value, RpcError> {
    let root = crate::config::account_dir(&account.name);
    let index = load_index(account)?;
    Ok(json!({
        "account": account.name,
        "total": index.contacts.len(),
        "sent_to": index.contacts.values().map(|c| u64::from(c.sent_to)).sum::<u64>(),
        "sent_cc": index.contacts.values().map(|c| u64::from(c.sent_cc)).sum::<u64>(),
        "received": index.contacts.values().map(|c| u64::from(c.received)).sum::<u64>(),
        "built_at": index.built_at,
        "cache_path": cache_path(&root).display().to_string(),
        "top": rows(&search(&index, "", TOP_ROWS)),
    }))
}

/// Every matched contact as the row the wire carries.
fn rows(matches: &[MatchResult<'_>]) -> Vec<Value> {
    matches
        .iter()
        .map(|m| {
            json!({
                "address": m.contact.address,
                "display_name": m.contact.display_name,
                "sent_to": m.contact.sent_to,
                "sent_cc": m.contact.sent_cc,
                "received": m.contact.received,
                "score": m.score,
            })
        })
        .collect()
}

/// The cached index, built on demand when there is none.
///
/// [`crate::contacts_cmd`]'s `load_or_build`, moved: the guard that refuses to
/// replace a populated cache with a thin rebuild (#0067) keeps what is on
/// disk, so a refusal shows the disk rather than the rebuild that was just
/// refused. The refusal is logged rather than printed, because a daemon has no
/// stderr a user reads.
fn load_index(account: &AccountConfig) -> Result<ContactIndex, RpcError> {
    let root = crate::config::account_dir(&account.name);
    let opening = |e: anyhow::Error| internal(format!("{e:#}"));
    if let Some(index) = load_cache(&root).map_err(opening)? {
        return Ok(index);
    }
    let index = build_index_for_account(account).map_err(opening)?;
    match save_rebuilt_cache(&root, &index).map_err(opening)? {
        CacheSave::Written => Ok(index),
        refused => {
            log::warn!(
                "[daemon] contacts rebuild for {} refused ({refused:?}); serving the cache at {}",
                account.name,
                cache_path(&root).display()
            );
            Ok(load_cache(&root).map_err(opening)?.unwrap_or(index))
        }
    }
}

// ---------------------------------------------------------------------------
// The rebuild
// ---------------------------------------------------------------------------

/// One rebuild, reported under [`REBUILD_PHASE`] so a batch of five accounts is
/// legible while it runs, and settled with the verdict `save_rebuilt_cache`
/// reached.
async fn rebuild(account: AccountConfig, handle: OperationHandle) {
    handle.set_running();
    handle.report(Progress {
        phase: REBUILD_PHASE.to_string(),
        done: 0,
        total: None,
        message: Some(account.name.clone()),
    });
    let built = tokio::task::spawn_blocking(move || rebuild_blocking(&account)).await;
    match built {
        Ok(Ok(result)) => handle.succeed(result),
        Ok(Err(e)) => handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => handle.fail(DomainError::internal(format!("the rebuild task {e}"))),
    }
}

/// The blocking half: walk the store, rank, and offer the result to the cache.
fn rebuild_blocking(account: &AccountConfig) -> anyhow::Result<Value> {
    let root = crate::config::account_dir(&account.name);
    let index = build_index_for_account(account)?;
    let contacts = index.contacts.len();
    let (saved, kept) = match save_rebuilt_cache(&root, &index)? {
        CacheSave::Written => ("written", 0),
        CacheSave::RefusedEmpty { kept } => ("refused_empty", kept),
        CacheSave::RefusedShrunk { kept, .. } => ("refused_shrunk", kept),
    };
    Ok(json!({
        "account": account.name,
        "contacts": contacts,
        "kept": kept,
        "saved": saved,
        "cache_path": cache_path(&root).display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// The array is the family, in method-name order, and every one of it is
    /// durable: a cache nobody chose is what an abandoned rebuild leaves.
    #[test]
    fn the_family_declares_three_durable_methods_in_name_order() {
        let names: Vec<&str> = CONTACT_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for spec in CONTACT_METHOD_SPECS {
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            assert_eq!(spec.since, 1, "{}", spec.name);
        }
    }
}
