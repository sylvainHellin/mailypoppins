//! The `diagnostic.*` family: health, the daemon's own log, the support bundle
//! (P6-U8) and the retention sweep (P4-U14).
//!
//! ```text
//! diagnostic.health        {}      -> {instance_id, version, protocol_version,
//!                                      uptime_secs, pid, socket, log_path,
//!                                      clients, accounts, holds, operations,
//!                                      store, checks}
//! diagnostic.log_path      {}      -> {path}
//! diagnostic.logs  {lines?, level?, since?}
//!                                  -> {path, lines, truncated}
//! diagnostic.store_gc  {account, dry_run, force}
//!                          -> {operation_id}
//!                          settles {account, dry_run, cap_bytes, before_bytes,
//!                                   after_bytes, evicted_bytes, evicted,
//!                                   decision}
//! diagnostic.support_bundle  {out?, redact?}
//!                          -> {operation_id}
//!                          settles {path, files, redactions}
//! ```
//!
//! The three reads are queries. The bundle is an
//! [`MethodKind::Operation`] with
//! [`CancelScope::Durable`](super::super::dispatch::CancelScope::Durable),
//! because a bundle is asked for by a client that is about to send it somewhere
//! and may well close its window while the daemon is still copying, and a
//! half-written bundle directory is worse than none. What each one answers is
//! assembled by [`crate::daemon::diagnostics`]; this module is the routing.
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

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::daemon::diagnostics::{
    self, BundlePlan, Diagnostics, LogQuery, DEFAULT_LOG_LINES, LOG_LEVELS, MAX_LOG_LINES,
};
use crate::store::sweep::{sweep, SweepDecision, SweepOptions, SweepOutcome};
use crate::store::{BlobStore, Store};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::state::ConnectionId;
use super::{internal, invalid_params, only_params, string_param};

/// The five methods of the family, in method-name order.
pub const DIAGNOSTIC_METHOD_SPECS: [MethodSpec; 5] = [
    MethodSpec::new("diagnostic.health", MethodKind::Query, 1),
    MethodSpec::new("diagnostic.log_path", MethodKind::Query, 1),
    MethodSpec::new("diagnostic.logs", MethodKind::Query, 1),
    MethodSpec::new("diagnostic.store_gc", MethodKind::Operation, 1),
    MethodSpec::new("diagnostic.support_bundle", MethodKind::Operation, 1),
];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
    diagnostics: Arc<Diagnostics>,
) {
    for spec in DIAGNOSTIC_METHOD_SPECS {
        dispatcher.register(Arc::new(DiagnosticMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
            diagnostics: Arc::clone(&diagnostics),
        }));
    }
}

/// The served method, selected by its own [`MethodSpec`].
pub struct DiagnosticMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
    diagnostics: Arc<Diagnostics>,
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
            match self.spec.name {
                "diagnostic.health" => {
                    only_params("diagnostic.health", &params, &[])?;
                    Ok(Outcome::query(self.diagnostics.health(ctx.protocol)))
                }
                "diagnostic.log_path" => {
                    only_params("diagnostic.log_path", &params, &[])?;
                    Ok(Outcome::query(
                        json!({"path": diagnostics::log_file().display().to_string()}),
                    ))
                }
                "diagnostic.logs" => {
                    let query = log_query(&params)?;
                    let path = diagnostics::log_file();
                    let answer =
                        tokio::task::spawn_blocking(move || diagnostics::read_logs(&path, &query))
                            .await
                            .map_err(|e| DomainError::internal(format!("the log read {e}")))?
                            .map_err(|e| internal(format!("{e:#}")))?;
                    Ok(Outcome::query(answer))
                }
                "diagnostic.support_bundle" => {
                    let plan = bundle_plan(&params, ctx.protocol)?;
                    let (id, handle) = self.operations.start(
                        ConnectionId(ctx.connection_id),
                        self.spec.cancel_scope,
                        self.spec.name,
                    );
                    let diagnostics = Arc::clone(&self.diagnostics);
                    tokio::spawn(bundle(diagnostics, plan, handle));
                    Ok(Outcome::query(json!({"operation_id": id.as_str()})))
                }
                _ => {
                    let snapshot = self.config.snapshot();
                    let pass = plan(&params, &snapshot)?;
                    let (id, handle) = self.operations.start(
                        ConnectionId(ctx.connection_id),
                        self.spec.cancel_scope,
                        self.spec.name,
                    );
                    tokio::spawn(run(pass, handle));
                    Ok(Outcome::query(json!({"operation_id": id.as_str()})))
                }
            }
        })
    }
}

/// Validate one `diagnostic.logs` call.
///
/// A bound above the cap is `-32602` naming the cap rather than a silent clamp:
/// a caller asking for a million lines has misunderstood something, and
/// answering five thousand without a word hides it.
fn log_query(params: &Value) -> Result<LogQuery, RpcError> {
    only_params("diagnostic.logs", params, &["lines", "level", "since"])?;
    let lines = match params.get("lines") {
        None | Some(Value::Null) => DEFAULT_LOG_LINES,
        Some(value) => {
            let asked = value
                .as_u64()
                .ok_or_else(|| invalid_params(format!("lines is a whole number, not {value}")))?;
            if asked > MAX_LOG_LINES as u64 {
                return Err(invalid_params(format!(
                    "lines is at most {MAX_LOG_LINES}, and {asked} is more than that"
                )));
            }
            asked as usize
        }
    };
    let level = match params.get("level") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(diagnostics::level_from_wire)
                .ok_or_else(|| {
                    invalid_params(format!(
                        "level is one of {}, not {value}",
                        LOG_LEVELS.join(", ")
                    ))
                })?,
        ),
    };
    let since = match params.get("since") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .ok_or_else(|| {
                    invalid_params(format!("since is an RFC3339 instant, not {value}"))
                })?,
        ),
    };
    Ok(LogQuery {
        lines,
        level,
        since,
    })
}

/// Validate one `diagnostic.support_bundle` call.
///
/// `out` is absolute or it is `-32602`: the daemon's working directory is not
/// the caller's, which is P4-U2's rule for every path that crosses the socket.
fn bundle_plan(params: &Value, protocol: u32) -> Result<BundlePlan, RpcError> {
    only_params("diagnostic.support_bundle", params, &["out", "redact"])?;
    let out = match params.get("out") {
        None | Some(Value::Null) => diagnostics::default_bundle_path(),
        Some(value) => {
            let path = PathBuf::from(
                value
                    .as_str()
                    .ok_or_else(|| invalid_params(format!("out is a path, not {value}")))?,
            );
            if !path.is_absolute() {
                return Err(invalid_params(format!(
                    "out is an absolute path; {} is relative to a working directory the daemon \
                     does not share",
                    path.display()
                )));
            }
            path
        }
    };
    let redact = match params.get("redact") {
        None | Some(Value::Null) => true,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid_params(format!("redact is a boolean, not {value}")))?,
    };
    Ok(BundlePlan {
        out,
        redact,
        protocol,
    })
}

/// Write one bundle to its terminal state.
async fn bundle(diagnostics: Arc<Diagnostics>, plan: BundlePlan, handle: OperationHandle) {
    handle.set_running();
    let out = plan.out.display().to_string();
    match tokio::task::spawn_blocking(move || diagnostics::write_bundle(&diagnostics, &plan)).await
    {
        Ok(Ok(result)) => handle.succeed(result),
        Ok(Err(e)) => handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => handle.fail(DomainError::internal(format!(
            "the bundle write to {out} {e}"
        ))),
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

    /// The sweep is one of five, an operation, durable, served from protocol 1.
    #[test]
    fn the_sweep_is_a_durable_operation() {
        assert_eq!(DIAGNOSTIC_METHOD_SPECS.len(), 5);
        let spec = DIAGNOSTIC_METHOD_SPECS
            .iter()
            .find(|spec| spec.name == "diagnostic.store_gc")
            .copied()
            .expect("the family still serves the sweep");
        assert_eq!(spec.kind, MethodKind::Operation);
        assert_eq!(spec.cancel_scope, CancelScope::Durable);
        assert_eq!(spec.since, 1);
    }

    /// A bound above the cap names the cap, and an unknown level names the five
    /// words it takes.
    #[test]
    fn a_log_query_refuses_what_it_cannot_answer() {
        assert_eq!(
            log_query(&json!({}))
                .expect("no parameters is the default")
                .lines,
            DEFAULT_LOG_LINES
        );
        assert!(log_query(&json!({"lines": MAX_LOG_LINES})).is_ok());
        let over = log_query(&json!({"lines": MAX_LOG_LINES + 1})).expect_err("refused");
        assert!(over.message.contains(&MAX_LOG_LINES.to_string()));
        let level = log_query(&json!({"level": "chatty"})).expect_err("refused");
        assert!(level.message.contains("warn"));
        assert!(log_query(&json!({"since": "yesterday"})).is_err());
        assert!(log_query(&json!({"tail": 10})).is_err());
    }

    /// A relative `out` is refused, and an absent one lands under the data
    /// directory.
    #[test]
    fn a_bundle_plan_insists_on_an_absolute_out() {
        let default = bundle_plan(&json!({}), 1).expect("no parameters is a default bundle");
        assert!(default.out.is_absolute());
        assert!(default.redact, "redaction is the default");
        assert!(bundle_plan(&json!({"out": "bundle"}), 1).is_err());
        assert!(bundle_plan(&json!({"format": "tar.gz"}), 1).is_err());
        assert!(
            !bundle_plan(&json!({"redact": false}), 1)
                .expect("an explicit off")
                .redact
        );
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
