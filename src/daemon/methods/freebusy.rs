//! `calendar.freebusy` (PERSO-255, `docs/plans/calendar.md`, phase 2, slice
//! 7): when the people and the rooms of a meeting are busy, which the event
//! forms show while the user picks a time and `mp calendar freebusy` prints.
//!
//! ```text
//! calendar.freebusy {account, addresses, start, end, zone?, interval_minutes?}
//!                       -> {operation_id}
//!                       settles CalendarFreeBusyResult
//! ```
//!
//! An operation, since it waits on the server, and
//! [`CancelScope::ClientScoped`](super::super::dispatch::CancelScope): a form
//! that closes, or a client that goes, takes its query with it, and nothing
//! is written anywhere. The account's own address is asked first, so the
//! user's row covers the calendars a client has hidden.
//!
//! The refusals, before an operation starts: an account without a calendar
//! backend (an iMIP-only account has no server to ask), a backend whose
//! features lack `free-busy`, a range or an address list the server would
//! refuse ([`crate::calendar_freebusy::resolve_query`]), all `-32602`; and a
//! sign-in without the calendar scope, the re-login prompt as `-32603`. An
//! address the server has no free/busy for is not a refusal: its schedule
//! carries the `error`, and the others stand.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::calendar::CalendarFreeBusyParams;
use mp_protocol::RpcError;

use crate::calendar_backend::{
    backend_for, choose_backend, features, preflight_scope, BackendChoice, CalendarBackend,
    CalendarBackendError,
};
use crate::calendar_freebusy::{resolve_query, to_result, FreeBusyQuery};
use crate::calendar_model::CalendarFeature;
use crate::calendar_view::local_zone;
use crate::config::AccountConfig;

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::runtime::account::off_thread;
use super::super::state::ConnectionId;
use super::calendar::backend_error;
use super::{invalid_params, only_params};

/// The family's one method, an array of its own for the reason
/// [`super::calendar::CALENDAR_VIEW_METHOD_SPECS`] is one.
pub const FREEBUSY_METHOD_SPECS: [MethodSpec; 1] =
    [MethodSpec::new("calendar.freebusy", MethodKind::Operation, 1).client_scoped()];

/// Register `calendar.freebusy` on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
) {
    for spec in FREEBUSY_METHOD_SPECS {
        dispatcher.register(Arc::new(FreeBusyMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
        }));
    }
}

/// `calendar.freebusy`.
pub struct FreeBusyMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
}

impl Method for FreeBusyMethod {
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
            let (account, query) = plan(&params, &accounts)?;
            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                self.spec.cancel_scope,
                self.spec.name,
            );
            tokio::spawn(run(account, query, handle));
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

/// The account and the query, every refusal decided before an operation id
/// exists.
pub fn plan(
    params: &Value,
    accounts: &[AccountConfig],
) -> Result<(AccountConfig, FreeBusyQuery), RpcError> {
    only_params(
        "calendar.freebusy",
        params,
        &[
            "account",
            "addresses",
            "end",
            "interval_minutes",
            "start",
            "zone",
        ],
    )?;
    let request: CalendarFreeBusyParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("calendar.freebusy: {e}")))?;
    let name = request.account.clone();
    let account = super::account::configured_account(accounts, &name)?.clone();
    match choose_backend(&account) {
        Ok(BackendChoice::None) => {
            return Err(invalid_params(format!(
                "{name} has no calendar backend, so there is no server to ask for free/busy; \
                 free/busy needs an account whose calendar mp reads from the server"
            )))
        }
        Ok(_) => {}
        Err(CalendarBackendError::Other(e)) => return Err(invalid_params(format!("{e:#}"))),
        Err(e) => return Err(backend_error(&name, e)),
    }
    if !features(&account).contains(&CalendarFeature::FreeBusy) {
        return Err(invalid_params(format!(
            "The calendar backend of {name} offers no free/busy"
        )));
    }
    super::account::ready_account(accounts, &name)?;
    preflight_scope(&account).map_err(|e| backend_error(&name, e))?;
    let query =
        resolve_query(&request, &account.own_address(), local_zone()).map_err(invalid_params)?;
    Ok((account, query))
}

/// Ask the backend, unless the client went first: a cancel before or during
/// the call drops the query, and the registry has settled it already.
async fn run(account: AccountConfig, query: FreeBusyQuery, handle: OperationHandle) {
    handle.set_running();
    let name = account.name.clone();
    let token = handle.token.clone();
    let asked = query.clone();
    let answered = off_thread("the free/busy query", move || async move {
        let backend = backend_for(&account)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{} has no calendar backend", account.name))?;
        if token.is_cancelled() {
            return Ok::<_, CalendarBackendError>(None);
        }
        tokio::select! {
            _ = token.cancelled() => Ok(None),
            schedules = backend.free_busy(&asked) => schedules.map(Some),
        }
    })
    .await;
    match answered {
        Ok(Ok(None)) => {}
        Ok(Ok(Some(schedules))) => match serde_json::to_value(to_result(&name, &query, &schedules))
        {
            Ok(value) => handle.succeed(value),
            Err(e) => handle.fail(DomainError::internal(format!(
                "serialising the free/busy answer: {e}"
            ))),
        },
        Ok(Err(e)) => handle.fail(DomainError::from(backend_error(&name, e))),
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// The query dies with the client that asked: nothing it does outlives
    /// the form.
    #[test]
    fn the_query_is_a_client_scoped_operation() {
        let [spec] = FREEBUSY_METHOD_SPECS;
        assert_eq!(spec.name, "calendar.freebusy");
        assert_eq!(spec.kind, MethodKind::Operation);
        assert_eq!(spec.cancel_scope, CancelScope::ClientScoped);
        assert_eq!(spec.since, 1);
    }
}
