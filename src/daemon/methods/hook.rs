//! The `hook.*` family (#0135): what `mp hooks` asks about an account's mail
//! hooks.
//!
//! - `hook.list` (query): the configured hooks with their cursor and last run.
//! - `hook.test` (query): one hook checked against one stored message, each
//!   criterion with its verdict and the JSON the command would read. Runs
//!   nothing and moves no cursor.
//! - `hook.replay` (operation): run one hook for one stored message now,
//!   whatever its cursor says, behind the same `match` table a live run
//!   passes. Moves no cursor either, and records the run as the last one.
//!
//! The hook runs inside the daemon, with the daemon's environment, which is
//! the point: a replay behaves as the live run would have.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::{AccountConfig, HookConfig};
use crate::daemon::hooks::{self, state};
use crate::store::blobs::BlobStore;
use crate::store::Store;

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::state::ConnectionId;
use super::{internal, invalid_params, only_params, string_param};

/// The three methods of the family, in method-name order.
pub const HOOK_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("hook.list", MethodKind::Query, 1),
    MethodSpec::new("hook.replay", MethodKind::Operation, 1),
    MethodSpec::new("hook.test", MethodKind::Query, 1),
];

/// The parameters `hook.test` and `hook.replay` take.
const ADDRESSED: [&str; 6] = ["account", "hook", "selector", "id", "row_id", "mailbox"];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
) {
    for spec in HOOK_METHOD_SPECS {
        dispatcher.register(Arc::new(HookMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
        }));
    }
}

/// The served method, selected by its own [`MethodSpec`].
pub struct HookMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
}

impl Method for HookMethod {
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
            let accounts = self.config.accounts();
            match self.spec.name {
                "hook.list" => {
                    only_params("hook.list", &params, &["account"])?;
                    let name = string_param(&params, "account")?;
                    let account = super::account::configured_account(&accounts, &name)?.clone();
                    let answer = tokio::task::spawn_blocking(move || list(&account))
                        .await
                        .map_err(|e| DomainError::internal(format!("the hook list {e}")))??;
                    Ok(Outcome::query(answer))
                }
                "hook.test" => {
                    only_params("hook.test", &params, &ADDRESSED)?;
                    let (account, hook) = resolve(&accounts, &params)?;
                    let answer =
                        tokio::task::spawn_blocking(move || test(&account, &hook, &params))
                            .await
                            .map_err(|e| DomainError::internal(format!("the hook test {e}")))??;
                    Ok(Outcome::query(answer))
                }
                _ => {
                    only_params("hook.replay", &params, &ADDRESSED)?;
                    let (account, hook) = resolve(&accounts, &params)?;
                    // Addressed before the operation starts, so a selector
                    // that names nothing is the caller's `-32602` rather than
                    // a failed operation.
                    let row_id = {
                        let account = account.name.clone();
                        let params = params.clone();
                        tokio::task::spawn_blocking(move || {
                            let store = open(&account)?;
                            super::message::address(&params, &store, &account).map(|row| row.id)
                        })
                        .await
                        .map_err(|e| DomainError::internal(format!("the replay lookup {e}")))??
                    };
                    let (id, handle) = self.operations.start(
                        ConnectionId(ctx.connection_id),
                        self.spec.cancel_scope,
                        self.spec.name,
                    );
                    tokio::spawn(replay(account, hook, row_id, handle));
                    Ok(Outcome::query(json!({"operation_id": id.as_str()})))
                }
            }
        })
    }
}

/// The ready account and the hook the parameters name.
fn resolve(
    accounts: &[AccountConfig],
    params: &Value,
) -> Result<(AccountConfig, HookConfig), RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;
    let wanted = string_param(params, "hook")?;
    let hook = account
        .hooks
        .iter()
        .find(|hook| hook.name == wanted)
        .cloned()
        .ok_or_else(|| {
            let known: Vec<&str> = account
                .hooks
                .iter()
                .map(|hook| hook.name.as_str())
                .collect();
            invalid_params(if known.is_empty() {
                format!("{name} has no hooks")
            } else {
                format!("{name} has no hook {wanted:?}; it has {}", known.join(", "))
            })
        })?;
    Ok((account.clone(), hook))
}

fn open(account: &str) -> Result<Store, RpcError> {
    Store::open(crate::config::store_path(account))
        .map_err(|e| internal(format!("opening the store of {account}: {e:#}")))
}

/// The `result` of `hook.list`.
fn list(account: &AccountConfig) -> Result<Value, RpcError> {
    let state = hooks::load_state(&state::state_path(&account.name))
        .map_err(|e| internal(format!("{e:#}")))?;
    let entries: Vec<Value> = account
        .hooks
        .iter()
        .map(|hook| {
            let cursor = state.hooks.get(&hook.name);
            json!({
                "name": hook.name,
                "mailbox": hook.mailbox,
                "mailbox_key": hooks::mailbox_key(account, hook),
                "exec": hook.exec,
                "timeout_secs": hook.timeout_secs,
                "match": {
                    "authenticated_from": hook.criteria.authenticated_from,
                    "authserv_id": hook.criteria.authserv_id,
                    "accept_spf": hook.criteria.accept_spf,
                    "to": hook.criteria.to,
                    "subject": hook.criteria.subject,
                    "headers": hook.criteria.headers,
                },
                "cursor": cursor.map(|cursor| json!({
                    "armed_at": cursor.armed_at,
                    "uidvalidity": cursor.uidvalidity,
                    "last_uid": cursor.last_uid,
                    "fired": cursor.fired.len(),
                })),
                "last_run": cursor.and_then(|cursor| cursor.last_run.as_ref()).map(|run| json!({
                    "at": run.at,
                    "message_id": run.message_id,
                    "uid": run.uid,
                    "outcome": run.outcome,
                    "ok": run.ok,
                    "duration_ms": run.duration_ms,
                })),
            })
        })
        .collect();
    Ok(json!({"account": account.name, "hooks": entries}))
}

/// The `result` of `hook.test`.
fn test(account: &AccountConfig, hook: &HookConfig, params: &Value) -> Result<Value, RpcError> {
    let store = open(&account.name)?;
    let blobs = BlobStore::for_account(&account.name);
    let row = super::message::address(params, &store, &account.name)?;
    let state = hooks::load_state(&state::state_path(&account.name))
        .map_err(|e| internal(format!("{e:#}")))?;
    let fired_before = state
        .hooks
        .get(&hook.name)
        .is_some_and(|cursor| cursor.has_fired(&row.message_id));
    hooks::describe(&store, &blobs, &account.name, hook, &row, fired_before)
        .map_err(|e| invalid_params(format!("{e:#}")))
}

/// Run one replay to its terminal state.
async fn replay(account: AccountConfig, hook: HookConfig, row_id: i64, handle: OperationHandle) {
    handle.set_running();
    let name = account.name.clone();
    let prepare_hook = hook.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let store = Store::open(crate::config::store_path(&name))?;
        let blobs = BlobStore::for_account(&name);
        hooks::prepare(&store, &blobs, &name, &prepare_hook, row_id, true)
    })
    .await;
    let prepared = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(e)) => return handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => {
            return handle.fail(DomainError::internal(format!(
                "the replay of {} {e}",
                hook.name
            )))
        }
    };
    let message_id = prepared.message_id.clone();
    let outcome = hooks::fire(
        &account.name,
        &hook,
        prepared,
        &state::state_path(&account.name),
        true,
    )
    .await;
    handle.succeed(json!({
        "account": account.name,
        "hook": hook.name,
        "message_id": message_id,
        "outcome": outcome.outcome,
        "ok": outcome.ok,
        "duration_ms": outcome.duration.as_millis() as u64,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
    }));
}
