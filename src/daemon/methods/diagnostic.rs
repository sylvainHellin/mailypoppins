//! The `diagnostic.*` family, as far as the admin slice needs it: the
//! retention sweep (P4-U14).
//!
//! ```text
//! diagnostic.store_gc  {account, dry_run, force}
//!                          -> {operation_id}
//!                          settles {account, dry_run, cap_bytes, before_bytes,
//!                                   after_bytes, evicted_bytes, evicted,
//!                                   decision}
//! ```
//!
//! `mp store gc` is served here rather than under a `store.*` family because
//! plan section 3.0 declares no such family and `docs/parity-matrix.md` SYN-08
//! already writes this name down. The sweep is diagnostics-and-maintenance by
//! its own classification: it reports what a cache holds and evicts from it,
//! and the daemon runs the same sweep after every sync without anybody asking.
//!
//! It is an [`MethodKind::Operation`] because it walks the blob tree and
//! unlinks from it, and [`super::super::dispatch::CancelScope::Durable`]
//! because a sweep abandoned half way leaves blobs unlinked and rows saying
//! otherwise.
//!
//! The decision crosses verbatim, as `{kind, …}`: `under_cap` with the
//! `cleared_marker` flag, `warned_first_breach`, `refused_too_much` with the
//! bytes it would have reclaimed, and `evicted`. The client renders it; the
//! sweep's own rules (`ANO-5`) are library behaviour and routing does not touch
//! them.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::store::sweep::{sweep, SweepDecision, SweepOptions, SweepOutcome};
use crate::store::{BlobStore, Store};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::state::ConnectionId;
use super::{invalid_params, only_params, string_param};

/// The one method of the family this build serves.
pub const DIAGNOSTIC_METHOD_SPECS: [MethodSpec; 1] = [MethodSpec::new(
    "diagnostic.store_gc",
    MethodKind::Operation,
    1,
)];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
) {
    for spec in DIAGNOSTIC_METHOD_SPECS {
        dispatcher.register(Arc::new(DiagnosticMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
        }));
    }
}

/// The served method, selected by its own [`MethodSpec`].
pub struct DiagnosticMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
}

/// One validated sweep.
struct Pass {
    account: String,
    policy: crate::config::RetentionPolicy,
    options: SweepOptions,
}

impl Method for DiagnosticMethod {
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
            let pass = plan(&params, &snapshot)?;
            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                self.spec.cancel_scope,
                self.spec.name,
            );
            tokio::spawn(run(pass, handle));
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

/// Validate one call: the account, its store, and the policy the sweep acts on.
///
/// An unresolvable retention policy is `-32602` naming what is wrong with it,
/// which is what `mp store gc` has always reported it as.
fn plan(params: &Value, snapshot: &super::super::config::Snapshot) -> Result<Pass, RpcError> {
    only_params(
        "diagnostic.store_gc",
        params,
        &["account", "dry_run", "force"],
    )?;
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(&snapshot.accounts, &name)?;
    let policy = crate::config::retention_for(&snapshot.config, account)
        .map_err(|e| invalid_params(format!("{e:#}")))?;
    Ok(Pass {
        account: name,
        policy,
        options: SweepOptions {
            dry_run: flag(params, "dry_run"),
            force: flag(params, "force"),
        },
    })
}

/// An optional boolean parameter, absent meaning false.
fn flag(params: &Value, name: &str) -> bool {
    params.get(name).and_then(Value::as_bool).unwrap_or(false)
}

/// Run one sweep to its terminal state.
async fn run(pass: Pass, handle: OperationHandle) {
    handle.set_running();
    let name = pass.account.clone();
    match tokio::task::spawn_blocking(move || sweep_blocking(&pass)).await {
        Ok(Ok(result)) => handle.succeed(result),
        Ok(Err(e)) => handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => handle.fail(DomainError::internal(format!("the sweep of {name} {e}"))),
    }
}

/// The blocking half: open the store, sweep, and report what it decided.
fn sweep_blocking(pass: &Pass) -> anyhow::Result<Value> {
    let store = Store::open(crate::config::store_path(&pass.account))?;
    let blobs = BlobStore::for_account(&pass.account);
    let outcome = sweep(&store, &blobs, &pass.policy, pass.options)?;
    Ok(report(&pass.account, &outcome))
}

/// One sweep outcome, whole, because `report_sweep_outcome` renders every part
/// of it and a client cannot recompute any of them.
fn report(account: &str, outcome: &SweepOutcome) -> Value {
    json!({
        "account": account,
        "dry_run": outcome.dry_run,
        "cap_bytes": outcome.cap_bytes,
        "before_bytes": outcome.before_bytes,
        "after_bytes": outcome.after_bytes,
        "evicted_bytes": outcome.reclaimed_bytes(),
        "evicted": outcome
            .evicted
            .iter()
            .map(|blob| json!({
                "hash": blob.hash,
                "kind": blob.kind.as_str(),
                "size": blob.size,
                "newest_date": blob.newest_date,
                "past_horizon": blob.past_horizon,
            }))
            .collect::<Vec<_>>(),
        "decision": decision(&outcome.decision),
    })
}

/// The decision, as `{kind}` plus whatever that kind carries.
fn decision(decision: &SweepDecision) -> Value {
    match decision {
        SweepDecision::UnderCap { cleared_marker } => {
            json!({"kind": "under_cap", "cleared_marker": cleared_marker})
        }
        SweepDecision::WarnedFirstBreach => json!({"kind": "warned_first_breach"}),
        SweepDecision::RefusedTooMuch { would_evict_bytes } => {
            json!({"kind": "refused_too_much", "would_evict_bytes": would_evict_bytes})
        }
        SweepDecision::Evicted => json!({"kind": "evicted"}),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// One method, an operation, durable, served from protocol 1.
    #[test]
    fn the_sweep_is_a_durable_operation() {
        assert_eq!(DIAGNOSTIC_METHOD_SPECS.len(), 1);
        let spec = DIAGNOSTIC_METHOD_SPECS[0];
        assert_eq!(spec.name, "diagnostic.store_gc");
        assert_eq!(spec.kind, MethodKind::Operation);
        assert_eq!(spec.cancel_scope, CancelScope::Durable);
        assert_eq!(spec.since, 1);
    }

    /// Every branch of the decision names itself, and only the two that carry a
    /// number carry one.
    #[test]
    fn every_decision_carries_its_kind() {
        assert_eq!(
            decision(&SweepDecision::UnderCap {
                cleared_marker: true
            }),
            json!({"kind": "under_cap", "cleared_marker": true})
        );
        assert_eq!(
            decision(&SweepDecision::RefusedTooMuch {
                would_evict_bytes: 7
            }),
            json!({"kind": "refused_too_much", "would_evict_bytes": 7})
        );
        assert_eq!(
            decision(&SweepDecision::Evicted),
            json!({"kind": "evicted"})
        );
    }
}
