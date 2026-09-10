//! The `draft.*` family, which is `draft.approve` and nothing else yet
//! (P3b-U10).
//!
//! It is [`crate::draft::mark_as_approved`] behind the socket: a surgical
//! rewrite of the `status:` line, no re-serialisation, no event of its own. The
//! watcher notices the daemon's own write like any other and publishes the
//! `draft.changed` a client learns the new state from, so an approval through
//! the socket and an approval in `$EDITOR` look identical from the outside.
//!
//! Ids resolve through [`DraftWatch::resolve`], the watcher's settled
//! inventory, rather than through the drafts index, which lives in a store
//! behind an engine lock: the whole point of this unit is that watching and
//! approving drafts cost no lock.
//!
//! The three refusals: `-32005` for an account no configuration names,
//! `-32602` with `{account, id}` for an id nothing resolves to, and the
//! family's own `-32010` `draft_invalid` for a file that will not parse,
//! carrying the same payload the `draft.invalid` event does, so a client
//! renders the caller's refusal and the watcher's event with one piece of code.
//! A draft that has already been sent is `-32602` with `{account, id, status}`.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::events::{DraftInvalid, KIND_DRAFT_INVALID};
use mp_protocol::ErrorCode;

use crate::daemon::config::ConfigStore;
use crate::daemon::watch::{diagnostics_of, DraftWatch};

use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};

/// The one method this family serves.
pub const DRAFT_METHOD_SPECS: [MethodSpec; 1] =
    [MethodSpec::new("draft.approve", MethodKind::Command, 1)];

/// `draft.approve` as the dispatcher serves it.
pub struct DraftApprove {
    /// The live configuration, so a reload decides which accounts exist.
    pub config: Arc<ConfigStore>,
    /// The settled inventory an id resolves through.
    pub watch: Arc<DraftWatch>,
    /// The state whose revision a command reports.
    pub canonical: Arc<crate::daemon::state::CanonicalState>,
}

/// Register the family from the one array that declares it.
pub fn register(dispatcher: &mut Dispatcher, method: Arc<DraftApprove>) {
    dispatcher.register(method);
}

impl Method for DraftApprove {
    fn spec(&self) -> MethodSpec {
        DRAFT_METHOD_SPECS[0]
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let (account, id) = (
                string_param(&params, "account")?,
                string_param(&params, "id")?,
            );
            if !self
                .config
                .accounts()
                .iter()
                .any(|configured| configured.name == account)
            {
                return Err(DomainError::new(
                    ErrorCode::AccountUnknown,
                    format!("no account named {account}"),
                    Some(json!({ "account": account })),
                ));
            }
            let path = self.watch.resolve(&account, &id).ok_or_else(|| {
                DomainError::invalid_params(format!("{account} has no draft {id}"))
                    .with_data(json!({"account": account, "id": id}))
            })?;

            let draft = crate::draft::parse_email_draft(&path)
                .map_err(|_| refuse_unparseable(&account, &id, &path))?;
            let status = draft.frontmatter.status.to_string();
            if status == "sent" {
                return Err(
                    DomainError::invalid_params(format!("{id} has already been sent"))
                        .with_data(json!({"account": account, "id": id, "status": status})),
                );
            }
            crate::draft::mark_as_approved(&path).map_err(|e| {
                DomainError::internal(format!("approving {}: {e:#}", path.display()))
            })?;

            Ok(Outcome::command(
                json!({
                    "account": account, "id": id, "status": "approved",
                    "path": path.display().to_string(),
                }),
                self.canonical.revision().get(),
                vec![ResourceId::new(format!("draft:{account}/{id}"))],
            ))
        })
    }
}

/// `-32010`, carrying the diagnostics the watcher would have published: the
/// file is read again here rather than remembered, because the refusal is about
/// the file as it is now.
fn refuse_unparseable(account: &str, id: &str, path: &std::path::Path) -> DomainError {
    let payload = DraftInvalid {
        account: account.to_string(),
        id: id.to_string(),
        path: path.display().to_string(),
        diagnostics: diagnostics_of(path),
    };
    let message = payload
        .diagnostics
        .first()
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| format!("{} will not parse", path.display()));
    DomainError::new(
        ErrorCode::DraftInvalid,
        message,
        Some(
            serde_json::to_value(&payload).unwrap_or_else(|_| json!({"kind": KIND_DRAFT_INVALID})),
        ),
    )
}

/// A required string parameter, or `-32602` naming it.
fn string_param(params: &Value, name: &str) -> Result<String, DomainError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            DomainError::invalid_params(format!("{name} is a required string parameter"))
        })
}
