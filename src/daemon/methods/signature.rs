//! The `signature.*` family (#0131, completed by the daemon leftovers of
//! 2026-10-22).
//!
//! ```text
//! signature.list        {account}               -> {account, names, default}
//! signature.read        {name}                  -> {name, path, content}
//! signature.create      {name}                  -> {name, path, content: ""}
//! signature.rename      {account, old, new}     -> {account, names, default}
//! signature.delete      {account, name}         -> {account, names, default}
//! signature.set_default {account, name|null}    -> {account, names, default}
//! ```
//!
//! A signature is a file under `<config>/signatures/` and the per-account
//! default is app state, both read and written through [`crate::signatures`],
//! the module the draft writers resolve a signature with, so the listing a
//! client shows is the set `draft.create`'s `signature` resolves against.
//! Every client manages signatures through these methods: the TUI's `cs`
//! overlay, the desktop's Signatures dialog and `mp config show`'s listing.
//!
//! No method writes a signature's content: the editor does, on the `path`
//! `signature.read` and `signature.create` answer. The watcher publishes what
//! a write leaves on disk, `signature.changed` for a file written or created
//! and `signature.removed` for one deleted or renamed away, so a client
//! re-reads its listing on those two events whoever made the change.
//!
//! A refusal is `-32602` carrying [`crate::signatures`]' own sentence (a name
//! it refuses, a name already taken, a name that names nothing); the three
//! writers that answer a listing take the account it is about, which is
//! `-32005` when it is not configured. The writers are commands: their
//! `affected` names `signature:<name>` and the account, and they publish
//! nothing themselves, since the watcher's events are what every client
//! applies.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use mp_protocol::signature::{SignatureFile, SignatureListing};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::state::CanonicalState;
use super::{internal, invalid_params, only_params, string_param};

/// The family, in method-name order.
pub const SIGNATURE_METHOD_SPECS: [MethodSpec; 6] = [
    MethodSpec::new("signature.create", MethodKind::Command, 1),
    MethodSpec::new("signature.delete", MethodKind::Command, 1),
    MethodSpec::new("signature.list", MethodKind::Query, 1),
    MethodSpec::new("signature.read", MethodKind::Query, 1),
    MethodSpec::new("signature.rename", MethodKind::Command, 1),
    MethodSpec::new("signature.set_default", MethodKind::Command, 1),
];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
) {
    for spec in SIGNATURE_METHOD_SPECS {
        dispatcher.register(Arc::new(SignatureMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
        }));
    }
}

/// One method of the family, selected by its own [`MethodSpec`].
pub struct SignatureMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
}

impl Method for SignatureMethod {
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
            let method = self.spec.name;
            match method {
                "signature.list" => {
                    only_params(method, &params, &["account"])?;
                    let account = self.account(&params)?;
                    let listing = blocking(move || Ok(listing(account))).await?;
                    Ok(Outcome::query(to_value(method, listing)?))
                }
                "signature.read" => {
                    only_params(method, &params, &["name"])?;
                    let name = string_param(&params, "name")?;
                    let file = blocking(move || {
                        refused(crate::signatures::validate_name(&name))?;
                        let content = crate::signatures::read(&name).ok_or_else(|| {
                            DomainError::from(invalid_params(format!(
                                "no signature named '{name}'"
                            )))
                        })?;
                        Ok(file(&name, content))
                    })
                    .await?;
                    Ok(Outcome::query(to_value(method, file)?))
                }
                "signature.create" => {
                    only_params(method, &params, &["name"])?;
                    let name = string_param(&params, "name")?;
                    let created = name.clone();
                    let file = blocking(move || {
                        refused(crate::signatures::create(&name).map(|_| ()))?;
                        Ok(file(&name, String::new()))
                    })
                    .await?;
                    let affected = vec![signature_resource(&created)];
                    Ok(self.command(to_value(method, file)?, affected))
                }
                "signature.rename" => {
                    only_params(method, &params, &["account", "new", "old"])?;
                    let account = self.account(&params)?;
                    let old = string_param(&params, "old")?;
                    let new = string_param(&params, "new")?;
                    let affected = vec![
                        signature_resource(&old),
                        signature_resource(&new),
                        account_resource(&account),
                    ];
                    let listing = blocking(move || {
                        refused(crate::signatures::rename(&old, &new))?;
                        Ok(listing(account))
                    })
                    .await?;
                    Ok(self.command(to_value(method, listing)?, affected))
                }
                "signature.delete" => {
                    only_params(method, &params, &["account", "name"])?;
                    let account = self.account(&params)?;
                    let name = string_param(&params, "name")?;
                    let affected = vec![signature_resource(&name), account_resource(&account)];
                    let listing = blocking(move || {
                        refused(crate::signatures::delete(&name))?;
                        Ok(listing(account))
                    })
                    .await?;
                    Ok(self.command(to_value(method, listing)?, affected))
                }
                _ => {
                    only_params(method, &params, &["account", "name"])?;
                    let account = self.account(&params)?;
                    let name = match params.get("name") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(name)) => Some(name.clone()),
                        Some(_) => {
                            return Err(invalid_params("name is a string or null").into());
                        }
                    };
                    let affected = vec![account_resource(&account)];
                    let listing = blocking(move || {
                        refused(crate::signatures::set_default_signature(
                            &account,
                            name.as_deref(),
                        ))?;
                        Ok(listing(account))
                    })
                    .await?;
                    Ok(self.command(to_value(method, listing)?, affected))
                }
            }
        })
    }
}

impl SignatureMethod {
    /// The configured account `params` names: any configured account, store
    /// or not, since signatures are configuration, like a drafts directory.
    fn account(&self, params: &Value) -> Result<String, DomainError> {
        let name = string_param(params, "account")?;
        let accounts = self.config.accounts();
        Ok(super::account::configured_account(&accounts, &name)?
            .name
            .clone())
    }

    /// A writer's outcome: the revision the daemon is at, and what it touched.
    fn command(&self, result: Value, affected: Vec<ResourceId>) -> Outcome {
        Outcome::command(result, self.canonical.revision().get(), affected)
    }
}

/// `account`'s listing as [`crate::signatures`] reads it now.
fn listing(account: String) -> SignatureListing {
    SignatureListing {
        names: crate::signatures::list(),
        default: crate::signatures::default_signature_name(&account),
        account,
    }
}

/// One signature file as `signature.read` and `signature.create` answer it.
fn file(name: &str, content: String) -> SignatureFile {
    SignatureFile {
        name: name.to_string(),
        path: crate::signatures::signature_file(name)
            .display()
            .to_string(),
        content,
    }
}

/// A refusal of [`crate::signatures`] as `-32602` with its own sentence.
fn refused(result: anyhow::Result<()>) -> Result<(), DomainError> {
    result.map_err(|e| invalid_params(format!("{e:#}")).into())
}

fn signature_resource(name: &str) -> ResourceId {
    ResourceId::new(format!("signature:{name}"))
}

fn account_resource(account: &str) -> ResourceId {
    ResourceId::new(format!("account:{account}"))
}

/// Run a file operation off the dispatcher's thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DomainError> + Send + 'static,
) -> Result<T, DomainError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| DomainError::internal(format!("the signature task {e}")))?
}

fn to_value<T: serde::Serialize>(method: &str, answer: T) -> Result<Value, DomainError> {
    serde_json::to_value(answer).map_err(|e| internal(format!("serialising {method}: {e}")).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::dispatch::CancelScope;

    /// The family, in method-name order, every one of it durable and served
    /// since protocol 1, the readers queries and the writers commands.
    #[test]
    fn the_family_declares_six_methods_in_name_order() {
        let names: Vec<&str> = SIGNATURE_METHOD_SPECS.iter().map(|s| s.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for spec in SIGNATURE_METHOD_SPECS {
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            let reads = matches!(spec.name, "signature.list" | "signature.read");
            let kind = if reads {
                MethodKind::Query
            } else {
                MethodKind::Command
            };
            assert_eq!(spec.kind, kind, "{}", spec.name);
        }
    }
}
