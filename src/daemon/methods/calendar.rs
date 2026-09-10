//! The `calendar.*` family: the organiser-side fold, and the attendee's reply
//! (P4-U14).
//!
//! ```text
//! calendar.events   {account}
//!                       -> {account, events:[AgendaEvent]}
//! calendar.rebuild  {account}
//!                       -> {operation_id}
//!                       settles {account, resolved, invites_seen,
//!                                replies_seen, cancelled}
//! calendar.rsvp     {account, selector|row_id, mailbox?, response}
//!                       -> {operation_id}
//!                       settles {account, selector, response, subject,
//!                                organizer, message_id, delivered}
//! ```
//!
//! Both are operations. A rebuild folds every stored `METHOD:REPLY` onto every
//! stored invitation of the account, which is a walk over a mailbox; an RSVP
//! *sends*, over SMTP and through the durable outbox, which is an unbounded
//! network round trip. Both are [`super::super::dispatch::CancelScope::Durable`]:
//! an RSVP abandoned half way is exactly the ambiguous submission
//! [`crate::outbox::sweep_pending_sends`] exists to park for a human.
//!
//! ## The refusals, and their order
//!
//! `ANO-4` first: a Graph account is refused in the sentence `mp invite` has
//! always printed, **before** anything about the selector is examined, so a
//! GUI can show the RSVP buttons disabled with their reason without naming a
//! resolvable message. Then the response word, then the selector's account,
//! then the row, then the invitation on it. Every one of them is `-32602`: the
//! caller's parameters were wrong, not the store.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::{AccountConfig, AuthMethod};
use crate::invite::Rsvp;
use crate::reconcile::reconcile_account;
use crate::store::{BlobStore, Store};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::runtime::account::off_thread;
use super::super::state::ConnectionId;
use super::{invalid_params, only_params, server_error, string_param};

/// `ANO-4`, in the sentence `src/main.rs` has printed for an RSVP since #0036.
pub const GRAPH_RSVP_REFUSAL: &str =
    "RSVP is not supported for Graph accounts yet (#0036, blocked on #0035)";

/// The two *operations* of the family, in method-name order.
pub const CALENDAR_METHOD_SPECS: [MethodSpec; 2] = [
    MethodSpec::new("calendar.rebuild", MethodKind::Operation, 1),
    MethodSpec::new("calendar.rsvp", MethodKind::Operation, 1),
];

/// The family's queries, an array of their own for the reason
/// [`super::message::MESSAGE_QUEUE_METHOD_SPECS`] is one (P5-U6):
/// `tests/daemon_admin_slice.rs` holds
/// `const _: () = assert!(CALENDAR_METHOD_SPECS.len() == CALENDAR_METHODS.len());`
/// over a two-name list, and growing that array would edit a pinned test to say
/// something it was not written to say. The split is a Rust-side fact: the
/// wire, the capability list and the dispatcher see three methods in one
/// family.
pub const CALENDAR_QUERY_METHOD_SPECS: [MethodSpec; 1] =
    [MethodSpec::new("calendar.events", MethodKind::Query, 1)];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
) {
    for spec in CALENDAR_METHOD_SPECS {
        dispatcher.register(Arc::new(CalendarMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
        }));
    }
    for spec in CALENDAR_QUERY_METHOD_SPECS {
        dispatcher.register(Arc::new(CalendarQueryMethod {
            spec,
            config: Arc::clone(&config),
        }));
    }
}

/// `calendar.events`: the account's agenda, deduped, folded and sorted.
///
/// A query, not an operation: it is one indexed join over `messages` plus one
/// blob read per invite row, which is the cost the TUI paid on its own UI
/// thread when it opened the store itself (#0034, #0038 item 6). What it is
/// *not* is a rebuild: it writes nothing, exactly as `calendar.rebuild` writes
/// nothing, because attendee statuses are derived where they are displayed.
pub struct CalendarQueryMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
}

impl Method for CalendarQueryMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let accounts = self.config.accounts();
            events(&params, &accounts)
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The `result` of `calendar.events`.
///
/// The agenda rows are built by [`crate::tui::app::calendar_view`], the one
/// place that owns the dedup, the tiebreak and the sort; a second copy here
/// would be a second answer to "which copy of this event is the row". That
/// module is still under `src/tui/`, which is where P5-U10's crate move has to
/// pick it up: see the ticket's follow-ups.
pub fn events(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    only_params("calendar.events", params, &["account"])?;
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;
    let self_address = crate::parse::extract_email_address(&account.default_from);

    let store =
        Store::open(crate::config::store_path(&name)).map_err(|e| server_error(&name, &e))?;
    let blobs = BlobStore::for_account(&name);
    let rows = crate::tui::app::calendar_view::load_events_for_account(
        &store,
        &blobs,
        &name,
        &self_address,
    );
    let events: Vec<mp_protocol::calendar::AgendaEvent> =
        rows.into_iter().map(|row| row.to_wire()).collect();
    Ok(json!({"account": name, "events": events}))
}

/// One of the two, selected by its own [`MethodSpec`].
pub struct CalendarMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
}

/// The work one operation of this family performs, decided before an operation
/// id exists so a refusal never arrives after one.
enum Work {
    Rebuild {
        account: String,
    },
    /// Boxed: the reply carries the account's whole configuration and the
    /// invitation's payload, and an enum every rebuild pays for that is a
    /// waste of the rebuild's stack frame.
    Rsvp(Box<RsvpWork>),
}

/// Everything one reply needs, read before the operation id exists.
struct RsvpWork {
    account: AccountConfig,
    selector: String,
    rsvp: Rsvp,
    response: String,
    ics: Vec<u8>,
    secrets: crate::secrets::SecretsBackendKind,
}

impl Method for CalendarMethod {
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
            let work = plan(self.spec.name, &params, &snapshot)?;
            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                self.spec.cancel_scope,
                self.spec.name,
            );
            tokio::spawn(run(work, handle));
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate one call and read everything the operation needs off disk.
fn plan(
    method: &str,
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    if method == "calendar.rebuild" {
        only_params(method, params, &["account"])?;
        let name = string_param(params, "account")?;
        super::account::ready_account(&snapshot.accounts, &name)?;
        return Ok(Work::Rebuild { account: name });
    }

    only_params(
        method,
        params,
        &["account", "mailbox", "response", "row_id", "selector"],
    )?;
    let name = string_param(params, "account")?;
    let account = super::account::configured_account(&snapshot.accounts, &name)?.clone();

    // `ANO-4` before anything else: the refusal must not depend on a selector
    // resolving, because the surface that shows it has no message in hand.
    if account.auth_method == AuthMethod::Graph {
        return Err(RpcError {
            code: super::INVALID_PARAMS,
            message: GRAPH_RSVP_REFUSAL.to_string(),
            data: Some(json!({"account": account.name})),
        });
    }

    let response = string_param(params, "response")?;
    let rsvp = match response.as_str() {
        "accept" => Rsvp::Accepted,
        "tentative" => Rsvp::Tentative,
        "decline" => Rsvp::Declined,
        other => {
            return Err(invalid_params(format!(
                "response is one of \"accept\", \"tentative\", \"decline\", not {other:?}"
            )))
        }
    };

    let addressed = |key: &str| !matches!(params.get(key), None | Some(Value::Null));
    if addressed("row_id") && addressed("selector") {
        return Err(invalid_params(
            "row_id and selector are two addresses; send exactly one",
        ));
    }
    if !addressed("row_id") {
        // Read before the store is opened, so a selector naming another
        // account is refused in the same order it always was.
        bound_to(&string_param(params, "selector")?, &account)?;
    }
    super::account::ready_account(&snapshot.accounts, &name)?;

    let store =
        Store::open(crate::config::store_path(&name)).map_err(|e| server_error(&name, &e))?;
    // `row_id` is the `messages.id` a `message.list` row carries, the address
    // P5-U4 gave `message.get` and P5-U6 gives this method: the TUI's agenda
    // row holds a `MessageRef` and nothing else (#0050), and re-deriving a
    // selector for it would be a whole-message read to answer a keypress.
    let (row, canonical) = if addressed("row_id") {
        let row_id = params
            .get("row_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid_params("row_id is a messages.id, which is an integer"))?;
        let row = crate::store::read::find_by_id(&store, row_id)
            .map_err(|e| server_error(&name, &e))?
            .ok_or_else(|| {
                invalid_params(format!("{name} holds no message with row id {row_id}"))
            })?;
        let canonical = crate::selector::Selector::for_message(&name, &row);
        (row, canonical)
    } else {
        let selector = string_param(params, "selector")?;
        let query = crate::selector::parse_in(
            &selector,
            crate::selector::Namespace::Received,
            &name,
            params.get("mailbox").and_then(Value::as_str),
        )
        .map_err(|e| invalid_params(format!("{e:#}")))?;
        crate::selector::resolve_received(&store, &query)
            .map_err(|e| invalid_params(format!("{e:#}")))?
    };
    // The invitation's own iMIP payload is the source of truth for the reply,
    // and it is a blob on the row (#0038 item 6).
    let blobs = BlobStore::for_account(&name);
    let ics = crate::store::read::load_invite_ics(&store, &blobs, row.id)
        .ok_or_else(|| invalid_params(format!("{canonical} carries no invitation to reply to")))?;

    Ok(Work::Rsvp(Box::new(RsvpWork {
        account,
        selector: canonical.to_string(),
        rsvp,
        response,
        ics,
        secrets: snapshot.config.secrets_backend,
    })))
}

/// A selector naming another account is refused rather than answered from the
/// wrong address: the reply goes out over *this* account's transport.
fn bound_to(selector: &str, account: &AccountConfig) -> Result<(), RpcError> {
    let parts = crate::selector::parse(selector).map_err(|e| invalid_params(format!("{e:#}")))?;
    match parts.account {
        Some(named) if named != account.name => Err(invalid_params(format!(
            "selector names account '{named}', but this reply is bound to '{}'",
            account.name
        ))),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/// Run one operation to its terminal state.
async fn run(work: Work, handle: OperationHandle) {
    handle.set_running();
    match work {
        Work::Rebuild { account } => {
            let name = account.clone();
            match tokio::task::spawn_blocking(move || fold(&account)).await {
                Ok(Ok(result)) => handle.succeed(result),
                Ok(Err(e)) => handle.fail(DomainError::internal(format!("{e:#}"))),
                Err(e) => handle.fail(DomainError::internal(format!(
                    "the calendar fold of {name} {e}"
                ))),
            }
        }
        Work::Rsvp(work) => {
            let RsvpWork {
                account,
                selector,
                rsvp,
                response,
                ics,
                secrets,
            } = *work;
            if let Err(e) = super::open_secrets(&account.name, secrets) {
                return handle.fail(DomainError::from(e));
            }
            let name = account.name.clone();
            let sent = off_thread("the RSVP", move || async move {
                submit(&account, &ics, rsvp).await
            })
            .await
            .and_then(|inner| inner);
            match sent {
                Ok(outcome) => handle.succeed(json!({
                    "account": name,
                    "selector": selector,
                    "response": response,
                    "subject": outcome.subject,
                    "organizer": outcome.organizer,
                    "message_id": outcome.message_id.unwrap_or_default(),
                    "delivered": outcome.send_result.any_succeeded(),
                })),
                Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
            }
        }
    }
}

/// `calendar.rebuild`: what the stored replies resolve on the stored
/// invitations. It writes nothing; attendee statuses are derived where they
/// are displayed (#0038 scope item 6).
fn fold(account: &str) -> anyhow::Result<Value> {
    let store = Store::open(crate::config::store_path(account))?;
    let blobs = BlobStore::for_account(account);
    let report = reconcile_account(&store, &blobs, account);
    Ok(json!({
        "account": account,
        "resolved": report.resolved,
        "invites_seen": report.invites_seen,
        "replies_seen": report.replies_seen,
        "cancelled": report.cancelled,
    }))
}

/// One reply, built from the invitation's own payload and submitted durably.
async fn submit(
    account: &AccountConfig,
    ics: &[u8],
    rsvp: Rsvp,
) -> anyhow::Result<crate::send::RsvpOutcome> {
    let smtp = super::send::smtp_config(account)?;
    crate::send::send_rsvp(ics, account, &account.default_from, rsvp, &smtp).await
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// The array is the family, in method-name order, and both of it are
    /// durable: a reply abandoned mid-submission is the state the outbox parks.
    #[test]
    fn the_family_declares_two_durable_operations_in_name_order() {
        let names: Vec<&str> = CALENDAR_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for spec in CALENDAR_METHOD_SPECS {
            assert_eq!(spec.kind, MethodKind::Operation, "{}", spec.name);
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            assert_eq!(spec.since, 1, "{}", spec.name);
        }
    }
}
