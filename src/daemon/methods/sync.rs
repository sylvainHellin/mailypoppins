//! The `sync.*` family: the two passes and the watch (P4-U10).
//!
//! `sync.quick` and `sync.full` are the two ends of one pass. Quick is the
//! newest UIDs per mailbox and takes a `limit`; full is everything the mailbox
//! lists and takes none, because a bounded full pass is a quick pass under
//! another name. Both are [`MethodKind::Operation`]s answering with an
//! `{operation_id}` and nothing else, and both are [`CancelScope::Durable`]: a
//! sync a GUI started must keep running, and be watchable, from the CLI window
//! beside it.
//!
//! `sync.watch` is the exception and is [`CancelScope::ClientScoped`]: a watch
//! exists to answer one client's question, so a `mp watch` the user interrupted
//! must not leave the daemon holding an IDLE on its behalf.
//!
//! ## What the client keeps
//!
//! `--all-accounts` is not on the wire. `account` is a required parameter of all
//! three methods and the loop over the configured accounts is the client's,
//! because the per-account header, the failure denominator and the exit code are
//! all rendering of a per-account result. An operation returning a map would
//! make the client re-derive an ordering the shape no longer carries, and would
//! make `operation.cancel` all-or-nothing where a `^C` stops the account
//! currently running.
//!
//! `--timeout` is not on the wire either: a client's patience is the client's.
//!
//! ## An account with nothing to sync is refused
//!
//! An account that configures neither IMAP nor SMTP has nothing to sync, and
//! that is [`ErrorCode::AccountNotReady`] with `state: "local_only"` rather than
//! an operation that does nothing. Issuing an id for work that will not happen
//! would be a lie the client then has to unpick. The check is here and not in
//! [`super::account::ready_account`] because a sync is what *gives* an account
//! its store: requiring one first would refuse every first sync.
//!
//! ## The five phases, and the drain reports
//!
//! A pass is [`run_tick_with_drains`]: the outbox then the mutation queue at the
//! head, the body, and both again at the tail (#0114). Each of the five slots is
//! one `operation.progress` report whose `phase` is
//! [`Phase::as_str`](crate::daemon::runtime::account::Phase::as_str); `done` and
//! `total` are the drain's own two counts (completed, and what it left behind),
//! present only when the drain has something a user would want printed, and
//! `message` carries the reason a drain could not run at all. That is the whole
//! of what `mp sync`'s `↻` lines are rendered from, and the tail label is
//! derived from the phase name.
//!
//! ## Where the pass runs
//!
//! Through [`tick_and_commit`] when this daemon holds the account's engine lock
//! and the request fits the runtime's tick, which is the seam the periodic
//! scheduler will use; otherwise through [`sync_mailboxes`], which takes the
//! engine lock for the length of the call and answers `Ok(None)` when another
//! process already holds it. A `mp sync` therefore keeps the #0122 degrade: a
//! blocked pass is a *successful* operation whose result says nothing ran, and
//! the client prints the skip line instead of a summary.
//!
//! The runtime's tick carries neither a mailbox subset nor `--dry-run`, so a
//! request that names either takes the guarded path even when a runtime is
//! live, and is then answered `blocked` by this daemon's own lock. Since P5-U8
//! every account has a runtime, so `mp sync --mailbox` and `mp sync --dry-run`
//! reach that corner on a daemon that is the account's engine; it is recorded
//! in `docs/lessons-learned.md` and in `BACKLOG.md`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::{ErrorCode, RpcError};

use crate::config::{AccountConfig, AuthMethod, ImapConfig};
use crate::daemon::runtime::account::{off_thread, Phase, TickKind, QUICK_TICK_LIMIT};
use crate::daemon::state::CanonicalState;
use crate::daemon::sync_outcome::from_sync_result;
use crate::sync::tick::run_tick_with_drains;
use crate::sync::{SyncResult, SyncTarget};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelScope, CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec,
    Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry, Progress};
use super::super::server::{tick_and_commit, RuntimeTable};
use super::super::state::ConnectionId;
use super::{invalid_params, string_param};

/// The three methods of the family, in method-name order.
pub const SYNC_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("sync.full", MethodKind::Operation, 1),
    MethodSpec::new("sync.quick", MethodKind::Operation, 1),
    MethodSpec::new("sync.watch", MethodKind::Operation, 1).client_scoped(),
];

/// The only mailbox this build's watcher watches (BACKLOG.md).
pub const WATCHED_MAILBOX: &str = "INBOX";

/// How long one IDLE round of a watch lasts before it is renewed.
///
/// The watch is a loop of bounded rounds rather than one unbounded IDLE so a
/// cancellation is observed within one round instead of never: the IMAP session
/// is driven on a thread of its own and cannot be dropped from under itself.
const WATCH_ROUND: Duration = Duration::from_secs(60);

/// What [`crate::imap_client::watch_mailbox`] returns when its round expired
/// without the mailbox changing.
const WATCH_TIMED_OUT: i32 = 2;

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    runtimes: Arc<RuntimeTable>,
    canonical: Arc<CanonicalState>,
    operations: Arc<OperationRegistry>,
) {
    for spec in SYNC_METHOD_SPECS {
        if spec.name == "sync.watch" {
            dispatcher.register(Arc::new(SyncWatch {
                config: Arc::clone(&config),
                operations: Arc::clone(&operations),
            }));
            continue;
        }
        dispatcher.register(Arc::new(SyncPass {
            spec,
            config: Arc::clone(&config),
            runtimes: Arc::clone(&runtimes),
            canonical: Arc::clone(&canonical),
            operations: Arc::clone(&operations),
        }));
    }
}

// ---------------------------------------------------------------------------
// sync.quick and sync.full
// ---------------------------------------------------------------------------

/// One of the two passes, selected by its own [`MethodSpec`].
pub struct SyncPass {
    /// Which of [`SYNC_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next pass.
    pub config: Arc<ConfigStore>,
    /// The account runtimes, so a pass this daemon is the engine for goes
    /// through the tick rather than contending with its own lock.
    pub runtimes: Arc<RuntimeTable>,
    /// Where a finished pass commits its outcome.
    pub canonical: Arc<CanonicalState>,
    /// The registry the operation is started in.
    pub operations: Arc<OperationRegistry>,
}

/// One validated pass request.
struct PassRequest {
    account: AccountConfig,
    kind: TickKind,
    /// The mailboxes `--mailbox` named, `None` for every configured one.
    mailbox: Option<Vec<String>>,
    dry_run: bool,
    limit: usize,
    secrets: crate::secrets::SecretsBackendKind,
}

impl Method for SyncPass {
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
            let request = pass_request(self.spec.name, &params, &snapshot)?;
            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                self.spec.cancel_scope,
                self.spec.name,
            );
            let runtimes = Arc::clone(&self.runtimes);
            let canonical = Arc::clone(&self.canonical);
            tokio::spawn(run_pass(request, handle, runtimes, canonical));
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

/// Validate one pass's parameters against the method that took them.
///
/// Strict about what it does not know: `all_accounts` on either method and
/// `limit` on `sync.full` are both `-32602` rather than silently ignored
/// parameters, because a caller that sent one disagrees with the contract and a
/// pass that quietly did something else is worse than a refusal.
fn pass_request(
    method: &str,
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<PassRequest, RpcError> {
    let quick = method == "sync.quick";
    let allowed: &[&str] = if quick {
        &["account", "limit", "mailbox", "dry_run"]
    } else {
        &["account", "mailbox", "dry_run"]
    };
    if let Some(object) = params.as_object() {
        if let Some(unexpected) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(invalid_params(format!(
                "{method} has no {unexpected} parameter; it takes {}",
                allowed.join(", ")
            )));
        }
    }
    let name = string_param(params, "account")?;
    let account = syncable_account(&snapshot.accounts, &name)?;

    let mailbox = match params.get("mailbox") {
        None | Some(Value::Null) => None,
        Some(Value::Array(names)) => Some(
            names
                .iter()
                .map(|name| {
                    name.as_str().map(str::to_string).ok_or_else(|| {
                        invalid_params("mailbox is an array of server mailbox names")
                    })
                })
                .collect::<Result<Vec<String>, RpcError>>()?,
        ),
        Some(_) => {
            return Err(invalid_params(
                "`--mailbox` repeats, so mailbox is an array of names on the wire",
            ))
        }
    };
    let limit = match params.get("limit") {
        None | Some(Value::Null) => {
            if quick {
                QUICK_TICK_LIMIT
            } else {
                usize::MAX
            }
        }
        Some(value) => value
            .as_u64()
            .and_then(|limit| usize::try_from(limit).ok())
            .ok_or_else(|| invalid_params("limit is a non-negative integer"))?,
    };
    let dry_run = match params.get("dry_run") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid_params("dry_run is a boolean")),
    };

    Ok(PassRequest {
        account: account.clone(),
        kind: if quick {
            TickKind::Quick
        } else {
            TickKind::Full
        },
        mailbox,
        dry_run,
        limit,
        secrets: snapshot.config.secrets_backend,
    })
}

/// The configured account behind `name`, once it has something to sync.
///
/// Unknown is `-32005`; configured with no server at all is `-32006` carrying
/// `state: "local_only"`, which is the client's cue to print the skip line and
/// keep the run's exit code at zero.
fn syncable_account<'a>(
    accounts: &'a [AccountConfig],
    name: &str,
) -> Result<&'a AccountConfig, RpcError> {
    let account = super::account::configured_account(accounts, name)?;
    if account.is_local_only() {
        return Err(RpcError {
            code: ErrorCode::AccountNotReady.code(),
            message: format!("{name} configures no server, so it has nothing to sync"),
            data: Some(json!({"account": name, "state": "local_only"})),
        });
    }
    Ok(account)
}

/// One pass, from the head drain to the committed outcome.
async fn run_pass(
    request: PassRequest,
    handle: OperationHandle,
    runtimes: Arc<RuntimeTable>,
    canonical: Arc<CanonicalState>,
) {
    handle.set_running();
    // Before the drains, exactly as `mp sync` resolved its targets before its
    // tick: a `--mailbox` nothing configures must not drain a queue on its way
    // to being refused.
    let targets = match resolve_targets(&request.account, request.mailbox.as_deref()) {
        Ok(targets) => targets,
        Err(e) => return handle.fail(DomainError::from(e)),
    };
    if let Err(e) = super::open_secrets(&request.account.name, request.secrets) {
        return handle.fail(DomainError::from(e));
    }

    // The runtime's tick when this daemon is the account's engine and the
    // request is one the tick can carry; the guarded pass otherwise.
    let runtime_tick = request.mailbox.is_none()
        && !request.dry_run
        && runtimes.get(request.account.name.as_str()).is_some();
    if runtime_tick {
        let outcome = tick_and_commit(&runtimes, &canonical, &request.account.name, request.kind)
            .await
            .filter(|outcome| !outcome.blocked);
        return match outcome {
            None => handle.succeed(blocked_result()),
            Some(outcome) => match outcome.error {
                Some(error) => handle.fail(DomainError::internal(error)),
                // The arrivals ride the outcome itself since P5-U8, so a pass
                // through the tick reports the same list a guarded pass does
                // and the answer carries it in the same place.
                None => {
                    let arrivals = outcome
                        .sync
                        .as_ref()
                        .map(|sync| json!(sync.new_inbox_mail))
                        .unwrap_or_else(|| json!([]));
                    handle.succeed(json!({
                        "blocked": false,
                        "outcome": outcome.sync,
                        "new_inbox_mail": arrivals,
                    }))
                }
            },
        };
    }

    let failed_mutations = Arc::new(AtomicU64::new(0));
    let head = || drain(&request, &handle, &failed_mutations, false);
    let tail = || drain(&request, &handle, &failed_mutations, true);
    let (_status, result) = run_tick_with_drains(
        head,
        || async {
            handle.report(Progress {
                phase: Phase::Body.as_str().to_string(),
                done: 0,
                total: None,
                message: None,
            });
            body(&request, &targets).await
        },
        tail,
    )
    .await;

    match result {
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
        // Another process is this account's engine (#0122): nothing ran, no
        // session was opened, and that is a success the client renders as a
        // skip rather than a summary of a pass that never happened.
        Ok(None) => handle.succeed(blocked_result()),
        Ok(Some(pass)) => {
            let arrivals = new_inbox_mail(&pass);
            let outcome = from_sync_result(
                &request.account.name,
                &pass,
                failed_mutations.load(Ordering::SeqCst),
                None,
            );
            if !request.dry_run {
                crate::contacts::hooks::bump_after_sync(&request.account, &pass.fresh_observations);
                // A dry run publishes nothing: an outcome counting what *would*
                // have been ingested would read, to every other client, as mail
                // that arrived.
                canonical.apply(super::super::state::Change::SyncCompleted(outcome.clone()));
            }
            handle.succeed(json!({
                "blocked": false,
                "outcome": outcome,
                "new_inbox_mail": arrivals,
            }));
        }
    }
}

/// The result of a pass another engine was already running (#0122).
fn blocked_result() -> Value {
    json!({"blocked": true, "outcome": null, "new_inbox_mail": []})
}

/// The inbox arrivals of one pass, as the desktop notification reads them
/// (#0009).
///
/// A sibling of `outcome` rather than a member of it, which is a statement
/// about this answer's shape and not about where the list travels: `outcome`
/// is the [`SyncCompleted`] summary as the event publishes it, and the
/// arrivals sit beside it so the client that asked for the pass reads them
/// without unpacking a summary.
///
/// Since P5-U8 the same list also rides the published `SyncCompleted` itself
/// ([`SyncCompleted::new_inbox_mail`], filled in `src/daemon/sync_outcome.rs`),
/// because an account runtime's tick has no caller to answer and the desktop
/// notification of #0009 would otherwise have died with the client-side
/// watcher. A subscribed client that asked for the pass therefore sees the
/// list twice, and drops one: `App::apply_tick` ignores a published tick for
/// an account whose pass this client is awaiting, so the answer notifies and
/// the event does not.
///
/// [`SyncCompleted`]: mp_protocol::events::SyncCompleted
fn new_inbox_mail(pass: &SyncResult) -> Value {
    Value::Array(
        pass.new_inbox_mail
            .iter()
            .map(|mail| json!({"from": mail.from, "subject": mail.subject}))
            .collect(),
    )
}

/// One end of the tick: the outbox, then the mutation queue, each reported as
/// its own phase.
///
/// The status suffix [`run_tick_with_drains`] wants is always empty here: this
/// path reports through `operation.progress`, and the string was only ever the
/// TUI's status line.
async fn drain(
    request: &PassRequest,
    handle: &OperationHandle,
    failed_mutations: &AtomicU64,
    tail: bool,
) -> String {
    let (outbox, mutations) = if tail {
        (Phase::TailOutbox, Phase::TailMutations)
    } else {
        (Phase::HeadOutbox, Phase::HeadMutations)
    };
    // A dry run touches nothing, so it drains nothing either: the queues hold
    // work the user has not asked this invocation to do.
    if request.dry_run {
        return String::new();
    }

    let account = Arc::new(request.account.clone());
    let drained = {
        let account = Arc::clone(&account);
        off_thread("the outbox drain", move || async move {
            crate::send::resume_outbox(&account).await
        })
        .await
    };
    let report = match &drained {
        Ok(drained) => (drained.completed > 0 || drained.still_open > 0).then(|| {
            (
                drained.completed as u64,
                (drained.still_open + drained.awaiting_submission) as u64,
            )
        }),
        Err(_) => None,
    };
    handle.report(Progress {
        phase: outbox.as_str().to_string(),
        done: report.map(|(completed, _)| completed).unwrap_or_default(),
        total: report.map(|(_, pending)| pending),
        message: None,
    });

    let ops = {
        let account = Arc::clone(&account);
        off_thread("the mutation-queue drain", move || async move {
            crate::pending_ops::resume_account(&account).await
        })
        .await
        .and_then(|inner| inner)
    };
    let (done, total, message) = match ops {
        Ok(Some(ops)) if ops.completed > 0 || ops.failed > 0 => {
            failed_mutations.fetch_add(ops.failed as u64, Ordering::SeqCst);
            (ops.completed as u64, Some(ops.failed as u64), None)
        }
        Ok(_) => (0, None, None),
        // Loud and not fatal: the queue is retried on the next tick, so a drain
        // that could not run may not turn a sync that worked into a failure.
        Err(e) => {
            log::warn!(
                "[pending_ops] draining {} at {mutations:?} failed: {e:#}",
                request.account.name
            );
            (0, None, Some(format!("{e:#}")))
        }
    };
    handle.report(Progress {
        phase: mutations.as_str().to_string(),
        done,
        total,
        message,
    });
    String::new()
}

/// The pass itself, guarded by the account's engine lock.
///
/// `Ok(None)` means another engine holds that lock. No body-fetch deadline is
/// passed on either transport (#0113): this is the explicit recovery path, and
/// the pass a user runs to make the store converge is the one pass that must
/// not stop early.
async fn body(request: &PassRequest, targets: &[SyncTarget]) -> anyhow::Result<Option<SyncResult>> {
    let account = Arc::new(request.account.clone());
    let targets = targets.to_vec();
    let (kind, limit, dry_run) = (request.kind, request.limit, request.dry_run);
    off_thread("the sync body", move || async move {
        if account.auth_method == AuthMethod::Graph {
            let graph = crate::config::GraphConfig::load(&account)?;
            // The Graph path is not guarded yet (#0122 covers the IMAP ingest),
            // so it always reports a pass that ran.
            return crate::graph::sync_mailboxes_graph(
                &graph,
                &account.name,
                &targets,
                limit,
                dry_run,
            )
            .await
            .map(Some);
        }
        let imap = ImapConfig::load(&account)?;
        let limit = match kind {
            TickKind::Quick => limit,
            TickKind::Full => usize::MAX,
        };
        crate::imap_client::sync_mailboxes(&imap, &account.name, &targets, limit, dry_run, None)
            .await
    })
    .await
    .and_then(|inner| inner)
}

/// The mailboxes one pass covers: the ones `--mailbox` named, or every
/// configured one.
///
/// Both halves of a target come from one configured mapping: building the role
/// from the typed string files an extra mailbox's rows under `projects` while
/// the rest of the product reads `Projects` (#0064).
fn resolve_targets(
    account: &AccountConfig,
    wanted: Option<&[String]>,
) -> Result<Vec<SyncTarget>, RpcError> {
    let Some(wanted) = wanted else {
        return Ok(crate::config::all_configured_mailboxes(account)
            .iter()
            .map(|(role, mapping)| SyncTarget {
                role: role.clone(),
                server_name: mapping.server.clone(),
            })
            .collect());
    };
    wanted
        .iter()
        .map(|name| {
            crate::config::find_sync_target(account, name)
                .map(|(role, server_name)| SyncTarget { role, server_name })
                .ok_or_else(|| {
                    invalid_params(format!(
                        "account '{}' has no mailbox '{name}' configured; it knows {}",
                        account.name,
                        crate::config::configured_mailbox_names(account)
                    ))
                })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// sync.watch
// ---------------------------------------------------------------------------

/// `sync.watch`: one IDLE on this build's one watched mailbox.
pub struct SyncWatch {
    /// The live configuration, so a reload is visible to the next watch.
    pub config: Arc<ConfigStore>,
    /// The registry the operation is started in.
    pub operations: Arc<OperationRegistry>,
}

impl Method for SyncWatch {
    fn spec(&self) -> MethodSpec {
        SYNC_METHOD_SPECS[2]
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let name = string_param(&params, "account")?;
            let account = super::account::configured_account(&snapshot.accounts, &name)?;
            match params.get("mailbox") {
                None | Some(Value::Null) => {}
                Some(Value::String(mailbox)) if mailbox.eq_ignore_ascii_case(WATCHED_MAILBOX) => {}
                Some(mailbox) => {
                    return Err(DomainError::invalid_params(format!(
                        "this daemon watches {WATCHED_MAILBOX} only; \
                         watching {mailbox} is not built (BACKLOG.md)"
                    ))
                    .with_data(json!({"account": name, "mailbox": mailbox})))
                }
            }
            if account.auth_method == AuthMethod::Graph {
                return Err(DomainError::internal(
                    "IMAP IDLE watch is not supported for Graph accounts. Use 'mp sync' instead.",
                ));
            }
            // Resolved before an id is issued, so `mp watch`'s refusal is the
            // call's error and the "Watching …" line is only ever printed over
            // a watch that started.
            super::open_secrets(&name, snapshot.config.secrets_backend)?;
            let imap = ImapConfig::load(account)
                .map_err(|e| DomainError::from(super::server_error(&name, &e)))?;

            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                CancelScope::ClientScoped,
                "sync.watch",
            );
            tokio::spawn(run_watch(imap, handle));
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

/// The watch: bounded IDLE rounds until the mailbox changes, the token is shut,
/// or the session fails.
///
/// The round bound is what makes the cancellation observable: the IMAP session
/// runs on a thread of its own, so the only place this task can notice a shut
/// token is between two rounds.
async fn run_watch(imap: ImapConfig, handle: OperationHandle) {
    handle.set_running();
    let imap = Arc::new(imap);
    loop {
        if handle.token.is_cancelled() {
            return;
        }
        let imap = Arc::clone(&imap);
        let round = off_thread("the IDLE watch", move || async move {
            crate::imap_client::watch_mailbox(&imap, WATCHED_MAILBOX, Some(WATCH_ROUND.as_secs()))
                .await
        })
        .await
        .and_then(|inner| inner);
        match round {
            Ok(WATCH_TIMED_OUT) => continue,
            Ok(_) => {
                return handle.succeed(json!({"mailbox": WATCHED_MAILBOX, "changed": true}));
            }
            Err(e) => return handle.fail(DomainError::internal(format!("{e:#}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The array is the family, in method-name order, with the one
    /// client-scoped method the plan names.
    #[test]
    fn the_family_declares_two_durable_passes_and_a_client_scoped_watch() {
        let names: Vec<&str> = SYNC_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        assert_eq!(names, vec!["sync.full", "sync.quick", "sync.watch"]);
        assert_eq!(SYNC_METHOD_SPECS[0].cancel_scope, CancelScope::Durable);
        assert_eq!(SYNC_METHOD_SPECS[1].cancel_scope, CancelScope::Durable);
        assert_eq!(SYNC_METHOD_SPECS[2].cancel_scope, CancelScope::ClientScoped);
    }

    /// Every phase name is distinct, which is what makes the tail label
    /// derivable from it.
    #[test]
    fn the_five_phase_names_are_distinct() {
        let mut names = [
            Phase::HeadOutbox.as_str(),
            Phase::HeadMutations.as_str(),
            Phase::Body.as_str(),
            Phase::TailOutbox.as_str(),
            Phase::TailMutations.as_str(),
        ];
        names.sort_unstable();
        let count = names.len();
        let mut unique = names.to_vec();
        unique.dedup();
        assert_eq!(unique.len(), count);
        assert!(Phase::TailOutbox.is_tail() && !Phase::HeadOutbox.is_tail());
    }
}
