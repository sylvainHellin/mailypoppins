//! The `send.*` family: the three sends and the operator's outbox (P4-U12).
//!
//! Everything that puts a message on the wire, and everything that answers for
//! one that is still on its way. Six methods:
//!
//! ```text
//! send.approved        {account}                 -> {operation_id}
//! send.draft           {account, selector}       -> {operation_id}
//! send.invite          {account, subject, start, …} -> {operation_id}
//! send.outbox_discard  {account, row_id}         -> {discarded, row_id, message_id, revision}
//! send.outbox_list     {account}                 -> OutboxListing
//! send.outbox_retry    {account, row_id}         -> {operation_id}
//! ```
//!
//! ## Everything here is [`CancelScope::Durable`]
//!
//! A send is the one thing in this program that cannot be undone by closing a
//! window. A submission abandoned because a client disconnected is exactly the
//! ambiguous state [`crate::outbox::sweep_pending_sends`] has to park for a
//! human, so manufacturing that state on every dropped socket would be the
//! opposite of what the durable outbox is for. A client that wants a send
//! stopped calls `operation.cancel`, which is a decision.
//!
//! ## The kinds, and the one that is not obvious
//!
//! `send.outbox_retry` is an [`MethodKind::Operation`] and not a command: it
//! re-arms one row and then submits it over SMTP and APPENDs it to the Sent
//! mailbox, which is an unbounded network round trip whose progress a GUI wants
//! to watch. A command that took thirty seconds and reported one revision would
//! be a lie about what it did. `send.outbox_discard` really is one committed
//! transaction against the local store, and `send.outbox_list` reads.
//!
//! ## What stays in the client
//!
//! The preview, the `[y/N]` prompt and `--all-accounts`. A daemon has no stdin,
//! and a daemon that asked a question would have to invent a way to be
//! answered; `--all-accounts` is a loop over configured accounts in
//! configuration order whose per-account summary and exit code are rendering, so
//! `account` is a required parameter here and a caller that sends
//! `all_accounts` is told rather than quietly served one account.
//!
//! The undo-send hold (`SND-04`) is in neither: `ANO-7` records that the CLI
//! send paths bypass it, Phase 6 moves it into the daemon and must keep that
//! true, so none of these methods takes a `hold`, a `hold_secs` or a
//! `countdown` and a caller that sends one is refused.
//!
//! ## `send.invite` takes three parameters the CLI adds
//!
//! `uid` so the UID the user read in the preview is the UID that goes out (the
//! client mints it while previewing, because the preview is client-side), and
//! `signature`/`no_signature` because the two global flags are the client's and
//! an invitation has no editable draft to carry a signature in the body of.
//! Absent, the daemon mints a UID and resolves the account's own signature.
//!
//! ## Which retries are allowed
//!
//! [`crate::outbox::retry`]'s rule is that only a `failed` row may be re-armed:
//! a row that is mid-flight cannot be re-armed under the send path's feet. One
//! state more is accepted here, and it is the one the dedup search exists for: a
//! row waiting for an APPEND that has *already been attempted* (`attempts > 0`)
//! is a row whose copy may or may not have been filed, and a retry re-drives
//! that APPEND behind the Message-ID search rather than re-arming anything. A
//! row nobody has attempted yet is refused, because somebody may be inside its
//! APPEND right now.
//!
//! One retry per account runs at a time ([`account_lock`]), and while one is
//! running the row's state is that retry's to decide, so a second call is
//! admitted rather than refused on a state it is watching change; its operation
//! then fails if the first one left nothing to do.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};

use mp_protocol::send::{
    ApprovedOutcome, OutboxCounts, OutboxListing, OutboxRetryOutcome, OutboxRow, RecipientOutcome,
    SendOutcome, SentCopy,
};
use mp_protocol::RpcError;

use crate::config::{AccountConfig, AuthMethod, EmailSettings, SmtpConfig};
use crate::daemon::runtime::account::off_thread;
use crate::outbox::{self, OutboxState};
use crate::store::drafts::DraftRow;
use crate::types::{EmailDraft, EmailFrontmatter, EmailStatus};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::operations::{OperationHandle, OperationRegistry, Progress};
use super::super::state::{CanonicalState, ConnectionId};
use super::{internal, invalid_params, server_error, string_param};

pub use crate::daemon::fake_transport::FAKE_TRANSPORT_ENV;

/// The six methods of the family, in method-name order.
pub const SEND_METHOD_SPECS: [MethodSpec; 6] = [
    MethodSpec::new("send.approved", MethodKind::Operation, 1),
    MethodSpec::new("send.draft", MethodKind::Operation, 1),
    MethodSpec::new("send.invite", MethodKind::Operation, 1),
    MethodSpec::new("send.outbox_discard", MethodKind::Command, 1),
    MethodSpec::new("send.outbox_list", MethodKind::Query, 1),
    MethodSpec::new("send.outbox_retry", MethodKind::Operation, 1),
];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
    operations: Arc<OperationRegistry>,
) {
    for spec in SEND_METHOD_SPECS {
        dispatcher.register(Arc::new(SendMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
            operations: Arc::clone(&operations),
        }));
    }
}

/// One of the six, selected by its own [`MethodSpec`].
pub struct SendMethod {
    /// Which of [`SEND_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next send.
    pub config: Arc<ConfigStore>,
    /// The state whose revision a command reports.
    pub canonical: Arc<CanonicalState>,
    /// The registry an operation is started in.
    pub operations: Arc<OperationRegistry>,
}

impl Method for SendMethod {
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
            let request = plan(self.spec.name, &params, &snapshot)?;
            match request {
                Request::List(listing) => {
                    Ok(Outcome::query(to_value("send.outbox_list", &listing)?))
                }
                Request::Discard { account, row_id } => {
                    let revision = self.canonical.revision().get();
                    // In the result as well as in the envelope: a command
                    // reports the revision it moved to, and a client that
                    // renders one row's disappearance reads it beside the row.
                    let mut result = discard(&account, row_id)?;
                    result["revision"] = json!(revision);
                    Ok(Outcome::command(
                        result,
                        revision,
                        vec![ResourceId::new(format!("outbox:{account}"))],
                    ))
                }
                Request::Operation(work) => {
                    let (id, handle) = self.operations.start(
                        ConnectionId(ctx.connection_id),
                        self.spec.cancel_scope,
                        self.spec.name,
                    );
                    tokio::spawn(run(*work, handle));
                    Ok(Outcome::query(json!({"operation_id": id.as_str()})))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One validated request: everything a refusal can be made of is decided here,
/// before an operation id exists, because a client that was handed an id for
/// work that cannot happen has to unpick a lie.
enum Request {
    List(OutboxListing),
    Discard {
        /// The account's name, which is all a discard reads it for.
        account: String,
        row_id: i64,
    },
    Operation(Box<Work>),
}

/// The work one operation performs.
enum Work {
    Draft {
        account: AccountConfig,
        row: DraftRow,
        email: EmailSettings,
        secrets: crate::secrets::SecretsBackendKind,
    },
    Approved {
        account: AccountConfig,
        email: EmailSettings,
        secrets: crate::secrets::SecretsBackendKind,
    },
    Invite {
        account: AccountConfig,
        plan: crate::invite::InvitePlan,
        signature: Option<String>,
        email: EmailSettings,
        secrets: crate::secrets::SecretsBackendKind,
    },
    Retry {
        account: AccountConfig,
        row_id: i64,
        secrets: crate::secrets::SecretsBackendKind,
    },
}

/// The parameters each method takes, and nothing else.
fn allowed(method: &str) -> &'static [&'static str] {
    match method {
        "send.approved" => &["account"],
        "send.draft" => &["account", "id", "selector"],
        "send.invite" => &[
            "account",
            "cc",
            "description",
            "duration",
            "end",
            "location",
            "no_signature",
            "signature",
            "start",
            "subject",
            "to",
            "uid",
        ],
        _ => &["account", "row_id"],
    }
}

/// Validate one call, and do the reading a query is.
fn plan(
    method: &str,
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Request, RpcError> {
    let allowed = allowed(method);
    if let Some(object) = params.as_object() {
        if let Some(unexpected) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(invalid_params(format!(
                "{method} has no {unexpected} parameter; it takes {}",
                allowed.join(", ")
            )));
        }
    }
    let name = string_param(params, "account")?;
    let account = super::account::configured_account(&snapshot.accounts, &name)?.clone();
    let secrets = snapshot.config.secrets_backend;
    let email = snapshot.config.email.clone();

    match method {
        "send.outbox_list" => Ok(Request::List(listing(&account.name)?)),
        "send.outbox_discard" => Ok(Request::Discard {
            row_id: existing_row(&account.name, params)?.id,
            account: account.name,
        }),
        "send.outbox_retry" => {
            let row = existing_row(&account.name, params)?;
            if !retryable(&row) && !retry_running(&account.name) {
                return Err(invalid_params(format!(
                    "outbox row {} is {}, and only a failed row can be retried",
                    row.id, row.state
                )));
            }
            Ok(Request::Operation(Box::new(Work::Retry {
                account,
                row_id: row.id,
                secrets,
            })))
        }
        "send.approved" => Ok(Request::Operation(Box::new(Work::Approved {
            account,
            email,
            secrets,
        }))),
        "send.invite" => {
            let request = crate::invite::InviteRequest {
                to: optional(params, "to"),
                cc: optional(params, "cc"),
                subject: optional(params, "subject"),
                start: optional(params, "start"),
                end: optional(params, "end"),
                duration: optional(params, "duration"),
                location: optional(params, "location"),
                description: optional(params, "description"),
            };
            let plan = crate::invite::plan_invite(
                &account,
                &request,
                params.get("uid").and_then(Value::as_str),
            )
            .map_err(|e| RpcError {
                code: super::INVALID_PARAMS,
                message: format!("{e}"),
                data: Some(json!({"account": account.name})),
            })?;
            let signature = super::draft::signature_of(&account, params, &email);
            Ok(Request::Operation(Box::new(Work::Invite {
                account,
                plan,
                signature,
                email,
                secrets,
            })))
        }
        _ => {
            let selector = optional(params, "selector");
            let id = optional(params, "id");
            if selector.is_none() && id.is_none() {
                return Err(invalid_params(
                    "send.draft sends one draft: name it with selector (or id)",
                ));
            }
            if let Some(selector) = selector.as_deref() {
                bound_to(selector, &account)?;
            }
            let key = super::draft::addressed_one(params, &account.name)?;
            let row = super::draft::resolve(&account.name, &key)?;
            Ok(Request::Operation(Box::new(Work::Draft {
                account,
                row,
                email,
                secrets,
            })))
        }
    }
}

/// An optional string parameter, absent when null.
fn optional(params: &Value, name: &str) -> Option<String> {
    params.get(name).and_then(Value::as_str).map(str::to_string)
}

/// A selector naming another account is refused rather than sent from the wrong
/// transport: the account and the selector are both on the wire and they have to
/// agree.
fn bound_to(selector: &str, account: &AccountConfig) -> Result<(), RpcError> {
    let parts = crate::selector::parse(selector).map_err(|e| invalid_params(format!("{e:#}")))?;
    match parts.account {
        Some(named) if named != account.name => Err(invalid_params(format!(
            "selector names account '{named}', but this send is bound to '{}'",
            account.name
        ))),
        _ => Ok(()),
    }
}

/// The row `row_id` names, or the sentence `mp outbox` has always refused with.
fn existing_row(account: &str, params: &Value) -> Result<crate::outbox::OutboxRow, RpcError> {
    let id = params
        .get("row_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid_params("row_id is a required integer parameter"))?;
    let missing = || invalid_params(format!("no outbox row {id}"));
    let path = crate::config::store_path(account);
    if !path.exists() {
        return Err(missing());
    }
    let store = crate::store::Store::open(&path).map_err(|e| server_error(account, &e))?;
    outbox::load(&store, id)
        .map_err(|e| server_error(account, &e))?
        .filter(|row| row.account == account)
        .ok_or_else(missing)
}

/// Whether a retry can do anything with this row: see the module docs.
fn retryable(row: &crate::outbox::OutboxRow) -> bool {
    match row.state {
        OutboxState::Failed => true,
        OutboxState::SentPendingAppend => row.attempts > 0,
        OutboxState::PendingSend | OutboxState::Done => false,
    }
}

// ---------------------------------------------------------------------------
// The outbox reads and the one write
// ---------------------------------------------------------------------------

/// The unfinished rows of one account, with everything the listing renders.
fn listing(account: &str) -> Result<OutboxListing, RpcError> {
    let path = crate::config::store_path(account);
    if !path.exists() {
        return Ok(OutboxListing {
            account: account.to_string(),
            ever_used: false,
            rows: Vec::new(),
            counts: OutboxCounts::default(),
        });
    }
    let store = crate::store::Store::open(&path).map_err(|e| server_error(account, &e))?;
    let rows = outbox::unfinished_rows(&store, account).map_err(|e| server_error(account, &e))?;
    let counts = outbox::counts(&store, account).map_err(|e| server_error(account, &e))?;
    Ok(OutboxListing {
        account: account.to_string(),
        ever_used: true,
        rows: rows.iter().map(listed_row).collect(),
        counts: OutboxCounts {
            open: counts.open,
            failed: counts.failed,
            partial: counts.partial,
        },
    })
}

/// One row as the listing carries it.
fn listed_row(row: &crate::outbox::OutboxRow) -> OutboxRow {
    // A `done` row is only listed when it kept a note, which is what a partial
    // delivery leaves behind (#0063): calling that `done` would bury the
    // recipient who never got it.
    let partial = row.state == OutboxState::Done;
    OutboxRow {
        id: row.id,
        state: row.state.as_str().to_string(),
        partial,
        never_submitted: row.state == OutboxState::PendingSend
            && row.submission_started_at.is_none(),
        message_id: row.message_id.clone(),
        target_mailbox: row.target_mailbox.clone(),
        updated: row.updated,
        last_error: row.last_error.clone(),
        rejected: row
            .envelope
            .as_ref()
            .map(|envelope| envelope.rejected.clone())
            .unwrap_or_default(),
        outstanding: row
            .envelope
            .as_ref()
            .map(|envelope| {
                envelope
                    .outstanding()
                    .into_iter()
                    .map(|(address, _)| address)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// `send.outbox_discard`: one row, one transaction, and the message-id in the
/// answer so the client can name what it dropped without having listed first.
fn discard(account: &str, row_id: i64) -> Result<Value, RpcError> {
    let store = crate::store::Store::open(crate::config::store_path(account))
        .map_err(|e| server_error(account, &e))?;
    let blobs = crate::store::BlobStore::for_account(account);
    let Some(row) = outbox::load(&store, row_id).map_err(|e| server_error(account, &e))? else {
        return Err(invalid_params(format!("no outbox row {row_id}")));
    };
    outbox::discard(&store, &blobs, row_id).map_err(|e| server_error(account, &e))?;
    Ok(json!({
        "discarded": true,
        "row_id": row_id,
        "message_id": row.message_id,
    }))
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/// One retry per account at a time, so two operators cannot re-arm and submit
/// the same row under each other's feet. The APPEND is guarded by the account's
/// engine lock either way (#0116); this is about the submission.
fn account_lock(account: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        locks
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

/// How many retries are admitted but not finished, per account.
static RETRIES: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether a retry for `account` has been admitted and has not finished.
fn retry_running(account: &str) -> bool {
    RETRIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(account)
        .is_some_and(|count| *count > 0)
}

/// A retry's admission, released however the operation ends.
struct RetryTicket(String);

impl RetryTicket {
    fn take(account: &str) -> RetryTicket {
        *RETRIES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(account.to_string())
            .or_insert(0) += 1;
        RetryTicket(account.to_string())
    }
}

impl Drop for RetryTicket {
    fn drop(&mut self) {
        let mut retries = RETRIES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = retries.get_mut(&self.0) {
            *count = count.saturating_sub(1);
        }
    }
}

/// Run one operation to its terminal state.
async fn run(work: Work, handle: OperationHandle) {
    handle.set_running();
    match work {
        Work::Draft {
            account,
            row,
            email,
            secrets,
        } => {
            if let Err(e) = super::open_secrets(&account.name, secrets) {
                return handle.fail(DomainError::from(e));
            }
            let name = account.name.clone();
            let selector = crate::selector::Selector::for_draft(&name, &row.id).to_string();
            let sent = off_thread("the send", move || async move {
                send_one(&account, &row.path, &email).await
            })
            .await
            .and_then(|inner| inner);
            match sent {
                Ok(outcome) => handle.succeed(value_or_fail(
                    "send.draft",
                    &SendOutcome {
                        selector: Some(selector),
                        ..outcome
                    },
                )),
                Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
            }
        }
        Work::Approved {
            account,
            email,
            secrets,
        } => {
            if let Err(e) = super::open_secrets(&account.name, secrets) {
                return handle.fail(DomainError::from(e));
            }
            let outcome = send_approved(account, email, &handle).await;
            handle.succeed(value_or_fail("send.approved", &outcome));
        }
        Work::Invite {
            account,
            plan,
            signature,
            email,
            secrets,
        } => {
            if let Err(e) = super::open_secrets(&account.name, secrets) {
                return handle.fail(DomainError::from(e));
            }
            let sent = off_thread("the invitation", move || async move {
                send_invite(&account, &plan, signature.as_deref(), &email).await
            })
            .await
            .and_then(|inner| inner);
            match sent {
                Ok(outcome) => handle.succeed(value_or_fail("send.invite", &outcome)),
                Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
            }
        }
        Work::Retry {
            account,
            row_id,
            secrets,
        } => {
            let _ticket = RetryTicket::take(&account.name);
            if let Err(e) = super::open_secrets(&account.name, secrets) {
                return handle.fail(DomainError::from(e));
            }
            let lock = account_lock(&account.name);
            let _held = lock.lock().await;
            let done = off_thread("the outbox retry", move || async move {
                retry_row(&account, row_id).await
            })
            .await
            .and_then(|inner| inner);
            match done {
                Ok(outcome) => handle.succeed(value_or_fail("send.outbox_retry", &outcome)),
                Err(e) => handle.fail(DomainError::invalid_params(format!("{e:#}"))),
            }
        }
    }
}

/// One outcome as its `result`, or the operation failing on a payload that
/// would not serialise (which is a bug here, not a bad request).
fn value_or_fail<T: Serialize>(method: &str, outcome: &T) -> Value {
    serde_json::to_value(outcome).unwrap_or_else(|e| {
        log::error!("[{method}] could not serialise its outcome: {e}");
        Value::Null
    })
}

/// A payload as JSON, refused as an internal error when it will not serialise.
fn to_value<T: Serialize>(method: &str, payload: &T) -> Result<Value, RpcError> {
    serde_json::to_value(payload)
        .map_err(|e| internal(format!("{method} could not serialise its answer: {e}")))
}

/// One draft, built, submitted and retired.
async fn send_one(
    account: &AccountConfig,
    path: &std::path::Path,
    email: &EmailSettings,
) -> anyhow::Result<SendOutcome> {
    let draft = crate::draft::parse_email_draft(path)?;
    crate::draft::validate_draft(&draft)?;
    let ctx = context_for(account, email)?;
    let sent = crate::send::send_draft(&draft, &ctx).await?;
    Ok(outcome_of(account, None, &sent.report, sent.settle_error))
}

/// The transport one account sends through, fake included.
fn context_for(
    account: &AccountConfig,
    email: &EmailSettings,
) -> anyhow::Result<crate::send::SendContext> {
    let is_graph = account.auth_method == AuthMethod::Graph;
    Ok(crate::send::SendContext {
        graph: is_graph
            .then(|| crate::config::GraphConfig::load(account))
            .transpose()?,
        smtp: (!is_graph).then(|| smtp_config(account)).transpose()?,
        account: account.clone(),
        email_settings: email.clone(),
        // The signature is already in the draft body (#0099).
        signature: None,
    })
}

/// The SMTP configuration, or the placeholder the fake transport needs: it *is*
/// the transport, so it has no credentials to load (P4-U12).
///
/// Shared with `calendar.rsvp` (P4-U14), which submits an iMIP reply over the
/// same transport and needs the same fake-transport escape hatch.
pub(crate) fn smtp_config(account: &AccountConfig) -> anyhow::Result<SmtpConfig> {
    match SmtpConfig::load(account) {
        Ok(config) => Ok(config),
        Err(_) if crate::daemon::fake_transport::armed() => Ok(SmtpConfig {
            host: "fake.invalid".to_string(),
            port: 465,
            username: String::new(),
            password: String::new(),
            default_from: account.default_from.clone(),
            accept_invalid_certs: false,
            auth_method: crate::config::AuthMethod::Password,
        }),
        Err(e) => Err(e),
    }
}

/// One send's report as the payload a client renders.
fn outcome_of(
    account: &AccountConfig,
    selector: Option<String>,
    report: &crate::send::SendReport,
    settle_error: Option<anyhow::Error>,
) -> SendOutcome {
    let files_copy = crate::config::appends_to_sent(account);
    SendOutcome {
        account: account.name.clone(),
        selector,
        message_id: String::new(),
        status_line: report.status_line(),
        recipients: report
            .send_result
            .results
            .iter()
            .map(|result| RecipientOutcome {
                address: result.address.clone(),
                role: result.role.to_string(),
                delivered: result.success,
                error: result.error.clone(),
            })
            .collect(),
        sent_copy: match (files_copy, report.state) {
            (false, _) => SentCopy::NotRequested,
            (true, Some(OutboxState::Done)) => SentCopy::Filed,
            (true, _) => SentCopy::Pending,
        },
        settle_error: settle_error.map(|e| format!("{e}")),
    }
}

/// `send.approved`: every approved draft of one account, in listing order, with
/// one progress report each so a batch of nine is visible while it runs.
async fn send_approved(
    account: AccountConfig,
    email: EmailSettings,
    handle: &OperationHandle,
) -> ApprovedOutcome {
    let name = account.name.clone();
    let rows: Vec<DraftRow> = crate::store::drafts::index_dir(&crate::config::drafts_dir(&name))
        .0
        .into_iter()
        .filter(|row| row.status == "approved")
        .collect();

    let total = rows.len() as u64;
    let mut outcome = ApprovedOutcome {
        account: name.clone(),
        results: Vec::new(),
        sent: 0,
        failed: 0,
    };
    for (index, row) in rows.into_iter().enumerate() {
        let selector = crate::selector::Selector::for_draft(&name, &row.id).to_string();
        handle.report(Progress {
            phase: "draft".to_string(),
            done: index as u64,
            total: Some(total),
            message: Some(selector.clone()),
        });
        let account = account.clone();
        let email = email.clone();
        let sent = off_thread("the batch send", move || async move {
            send_one(&account, &row.path, &email).await
        })
        .await
        .and_then(|inner| inner);
        let result = match sent {
            Ok(result) => SendOutcome {
                selector: Some(selector),
                ..result
            },
            // A draft that could not be sent is one line of the batch and not
            // the end of it: the loop keeps going, exactly as it always has.
            Err(e) => SendOutcome {
                account: name.clone(),
                selector: Some(selector),
                message_id: String::new(),
                status_line: format!("{e:#}"),
                recipients: Vec::new(),
                sent_copy: SentCopy::NotRequested,
                settle_error: None,
            },
        };
        if result.recipients.iter().any(|r| r.delivered) {
            outcome.sent += 1;
        } else {
            outcome.failed += 1;
        }
        outcome.results.push(result);
    }
    // One refresh for the whole batch rather than one per draft, which is what
    // `mp send-approved` has always paid.
    if let Err(e) = crate::store::drafts::refresh_account(&name) {
        log::warn!("could not refresh the drafts index of {name}: {e:#}");
    }
    outcome
}

/// `send.invite`: the `VEVENT`, the iMIP MIME tree and the durable submission.
async fn send_invite(
    account: &AccountConfig,
    plan: &crate::invite::InvitePlan,
    signature: Option<&str>,
    email: &EmailSettings,
) -> anyhow::Result<SendOutcome> {
    let ics = crate::invite::build_invite_ics(&plan.spec)?;
    let event =
        crate::calendar::parse_ics(ics.as_bytes()).map(|p| crate::calendar::event_frontmatter(&p));

    // A synthetic draft, used only to build the message: nothing is written to
    // disk on this path (the Sent copy is the outbox's, #0037).
    let sent_at = chrono::Utc::now();
    let draft = EmailDraft {
        path: std::path::PathBuf::from(format!(
            "{}-invite-{}.md",
            sent_at.format("%Y%m%d-%H%M%S"),
            crate::parse::slugify_subject(&plan.subject)
        )),
        frontmatter: EmailFrontmatter {
            id: None,
            date: None,
            to: plan.to_field.clone(),
            cc: plan.cc_field.clone(),
            bcc: None,
            subject: plan.subject.clone(),
            status: EmailStatus::Approved,
            from: Some(account.default_from.clone()),
            reply_to: None,
            attachments: None,
            sent_at: None,
            sent_via: None,
            message_id: None,
            in_reply_to: None,
            forwarded_from: None,
            signature: None,
            event,
        },
        body_markdown: plan.body.clone(),
    };

    let smtp = smtp_config(account)?;
    let built =
        crate::send::build_draft_message(&draft, &smtp.default_from, email, signature, Some(&ics))?;
    let report = crate::send::send_durably(&built, account, &smtp).await?;
    if report.send_result.any_succeeded() {
        crate::contacts::hooks::bump_after_send(account, &draft);
    }
    let mut outcome = outcome_of(account, None, &report, None);
    outcome.message_id = built.message_id.clone();
    Ok(outcome)
}

/// One retry: re-arm what can be re-armed, submit what was never submitted, and
/// drive the outstanding APPENDs against the operator's own clock.
async fn retry_row(account: &AccountConfig, row_id: i64) -> anyhow::Result<OutboxRetryOutcome> {
    let path = crate::config::store_path(&account.name);
    let store = crate::store::Store::open(&path)?;
    let row =
        outbox::load(&store, row_id)?.ok_or_else(|| anyhow::anyhow!("no outbox row {row_id}"))?;
    match row.state {
        OutboxState::Failed => outbox::retry(&store, row_id)?,
        // The APPEND was attempted and did not land: re-drive it, behind the
        // Message-ID search the attempt counter arms.
        OutboxState::SentPendingAppend if row.attempts > 0 => {}
        state => {
            anyhow::bail!("outbox row {row_id} is {state}, and only a failed row can be retried")
        }
    }
    // The store is reopened by the resume path, so this handle goes first.
    drop(store);

    // The operator named one row and means now, so the drain does not wait out
    // the backoff a failed attempt armed.
    let now = outbox::unix_now() + outbox::BACKOFF_MAX_SECS;
    let result = crate::send::resume_outbox_at(account, now).await;

    let store = crate::store::Store::open(&path)?;
    Ok(OutboxRetryOutcome {
        row_id,
        state: outbox::load(&store, row_id)?.map(|row| row.state.as_str().to_string()),
        completed: result.completed,
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// The array is the family, in method-name order, and every one of it is
    /// durable: a send outlives the client that asked for it.
    #[test]
    fn the_family_declares_six_durable_methods_in_name_order() {
        let names: Vec<&str> = SEND_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for spec in SEND_METHOD_SPECS {
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            assert_eq!(spec.since, 1, "{}", spec.name);
        }
    }

    /// A retry admitted while another is running holds a ticket, so the second
    /// call is queued behind the first instead of being refused on a state it
    /// is watching change.
    #[test]
    fn a_ticket_is_what_says_a_retry_is_running() {
        assert!(!retry_running("ticket-test"));
        {
            let _ticket = RetryTicket::take("ticket-test");
            assert!(retry_running("ticket-test"));
        }
        assert!(!retry_running("ticket-test"));
    }
}
