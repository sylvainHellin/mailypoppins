//! The `signature.*` family (#0131): `signature.list`, the one a compose
//! wizard needs.
//!
//! A signature is a file under `<config>/signatures/` and the per-account
//! default is app state, both read through [`crate::signatures`], the module
//! the TUI and the draft writers already use, so the listing a client shows
//! is the set `draft.create`'s `signature` resolves against. The rest of the
//! family (read, create, rename, delete, the default) and a
//! `signature.removed` event are still to come; until then a client writes
//! those through the same module.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use mp_protocol::signature::SignatureListing;

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
};
use super::{internal, only_params, string_param};

/// The family, in method-name order.
pub const SIGNATURE_METHOD_SPECS: [MethodSpec; 1] =
    [MethodSpec::new("signature.list", MethodKind::Query, 1)];

/// Register the family on `dispatcher`.
pub fn register(dispatcher: &mut Dispatcher, config: Arc<ConfigStore>) {
    for spec in SIGNATURE_METHOD_SPECS {
        dispatcher.register(Arc::new(SignatureMethod {
            spec,
            config: Arc::clone(&config),
        }));
    }
}

/// The served method, selected by its own [`MethodSpec`].
pub struct SignatureMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
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
            only_params("signature.list", &params, &["account"])?;
            let name = string_param(&params, "account")?;
            // Any configured account, store or not: signatures are
            // configuration, like a drafts directory.
            let accounts = self.config.accounts();
            let account = super::account::configured_account(&accounts, &name)?
                .name
                .clone();
            let listing = tokio::task::spawn_blocking(move || SignatureListing {
                names: crate::signatures::list(),
                default: crate::signatures::default_signature_name(&account),
                account,
            })
            .await
            .map_err(|e| DomainError::internal(format!("the signature listing {e}")))?;
            let answer = serde_json::to_value(listing)
                .map_err(|e| internal(format!("serialising signature.list: {e}")))?;
            Ok(Outcome::query(answer))
        })
    }
}
