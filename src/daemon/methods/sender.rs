//! The `sender.*` family: the blocked senders (PERSO-261).
//!
//! ```text
//! sender.block   {address, move_existing?}  -> {entry, added, entries, moved, skipped}
//! sender.unblock {address}                  -> {entry, removed, entries}
//! sender.list    {}                         -> {entries}
//! ```
//!
//! The list and the moves are [`crate::daemon::senders`]. `sender.block` adds
//! the entry and, unless `move_existing` is `false`, moves the sender's mail
//! in each account's Inbox to that account's Junk folder through the durable
//! queue, then asks each of those accounts for a drain, so the server half
//! runs as a `message.move` with `settle: false` does. An account with a
//! server and no Junk folder is named in `skipped` and never fails the call;
//! a local-only account has no mail arriving and is left out. `sender.unblock`
//! removes the entry and moves nothing back.
//!
//! An entry that is neither a full address nor `@domain` is `-32602` with the
//! sentence [`mp_core::blocklist::normalize`] refuses it with, and so is a
//! block of one of the user's own addresses or of its domain, which would
//! move the user's own mail to Junk ([`mp_core::blocklist::checked`]): the
//! own addresses are every configured account's `default_from`. Both writers publish
//! `sender.blocklist_changed` with the whole list when it changed, and the
//! counts of every account whose mail moved.

use std::sync::Arc;

use futures::future::BoxFuture;
use log::warn;
use serde_json::{json, Value};

use mp_protocol::events::KIND_SENDER_BLOCKLIST_CHANGED;
use mp_protocol::sender::{
    BlockMoved, BlockSkipped, BlockedSenders, SenderBlocked, SenderUnblocked,
};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::senders;
use super::super::server::RuntimeTable;
use super::super::state::events::Event;
use super::super::state::CanonicalState;
use super::{internal, invalid_params, only_params, string_param};

/// The family, in method-name order.
pub const SENDER_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("sender.block", MethodKind::Command, 1),
    MethodSpec::new("sender.list", MethodKind::Query, 1),
    MethodSpec::new("sender.unblock", MethodKind::Command, 1),
];

/// The resource both writers name: the one list.
const BLOCKLIST_RESOURCE: &str = "sender:blocklist";

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
    runtimes: Arc<RuntimeTable>,
) {
    for spec in SENDER_METHOD_SPECS {
        dispatcher.register(Arc::new(SenderMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
            runtimes: Arc::clone(&runtimes),
        }));
    }
}

/// One method of the family, selected by its own [`MethodSpec`].
pub struct SenderMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
    runtimes: Arc<RuntimeTable>,
}

/// The `sender.blocklist_changed` of a list that moved.
pub fn blocklist_changed(entries: &[String]) -> Event {
    Event::Lifecycle {
        kind: KIND_SENDER_BLOCKLIST_CHANGED,
        payload: serde_json::to_value(BlockedSenders {
            entries: entries.to_vec(),
        })
        .unwrap_or_else(|_| json!({"entries": entries})),
    }
}

impl Method for SenderMethod {
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
                "sender.list" => {
                    only_params(method, &params, &[])?;
                    let entries = blocking(|| {
                        senders::load(&senders::list_path())
                            .map_err(|e| DomainError::from(internal(format!("{e:#}"))))
                    })
                    .await?;
                    Ok(Outcome::query(to_value(
                        method,
                        BlockedSenders { entries },
                    )?))
                }
                "sender.block" => self.block(&params).await,
                _ => self.unblock(&params).await,
            }
        })
    }
}

impl SenderMethod {
    async fn block(&self, params: &Value) -> Result<Outcome, DomainError> {
        let method = self.spec.name;
        only_params(method, params, &["address", "move_existing"])?;
        let own = mp_core::blocklist::own_addresses(self.config.accounts().iter());
        let entry = mp_core::blocklist::checked(&string_param(params, "address")?, &own)
            .map_err(invalid_params)?;
        let move_existing = match params.get("move_existing") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(invalid_params("move_existing is a boolean").into()),
        };
        let accounts: Vec<_> = self
            .config
            .accounts()
            .iter()
            .filter(|account| !account.is_local_only())
            .cloned()
            .collect();
        let (edit, moved, skipped) = blocking(move || {
            let edit = senders::block(&senders::list_path(), &entry)
                .map_err(|e| DomainError::from(internal(format!("{e:#}"))))?;
            let mut moved = Vec::new();
            let mut skipped = Vec::new();
            for account in &accounts {
                let name = account.name.clone();
                let store = crate::store::open_store(&name);
                let catalogue = match &store {
                    Some(store) => crate::catalogue::Catalogue::from_store(store, account),
                    None => crate::catalogue::Catalogue::resolve(account, &[]),
                };
                if senders::junk_key(&catalogue).is_none() {
                    let reason = senders::no_junk_reason(&name);
                    warn!("[senders] blocking {entry}: {reason}");
                    skipped.push(BlockSkipped {
                        account: name,
                        reason,
                    });
                    continue;
                }
                let Some(store) = store.filter(|_| move_existing) else {
                    continue;
                };
                match senders::move_existing(&store, account, std::slice::from_ref(&entry)) {
                    Ok(senders::Moved::Rows { mailbox, count }) if count > 0 => {
                        moved.push(BlockMoved {
                            account: name,
                            mailbox,
                            count: count as u64,
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("[senders] blocking {entry} in {name}: {e:#}");
                        skipped.push(BlockSkipped {
                            reason: format!("{name}'s Inbox could not be moved: {e:#}"),
                            account: name,
                        });
                    }
                }
            }
            Ok((edit, moved, skipped))
        })
        .await?;

        for row in &moved {
            if let Some(runtime) = self.runtimes.get(&row.account) {
                runtime.request_drain();
            }
            let account = self
                .config
                .accounts()
                .iter()
                .find(|account| account.name == row.account)
                .cloned();
            if let Some(account) = account {
                super::super::runtime::publish::publish_changed_counts(&self.canonical, &account)
                    .await;
            }
        }
        if edit.changed {
            self.canonical.publish(blocklist_changed(&edit.entries));
        }
        let mut affected = vec![ResourceId::new(BLOCKLIST_RESOURCE)];
        affected.extend(
            moved
                .iter()
                .map(|row| ResourceId::new(format!("account:{}", row.account))),
        );
        let result = SenderBlocked {
            entry: edit.entry,
            added: edit.changed,
            entries: edit.entries,
            moved,
            skipped,
        };
        Ok(self.command(to_value(method, result)?, affected))
    }

    async fn unblock(&self, params: &Value) -> Result<Outcome, DomainError> {
        let method = self.spec.name;
        only_params(method, params, &["address"])?;
        let entry = entry_param(params)?;
        let edit = blocking(move || {
            senders::unblock(&senders::list_path(), &entry)
                .map_err(|e| DomainError::from(internal(format!("{e:#}"))))
        })
        .await?;
        if edit.changed {
            self.canonical.publish(blocklist_changed(&edit.entries));
        }
        let result = SenderUnblocked {
            entry: edit.entry,
            removed: edit.changed,
            entries: edit.entries,
        };
        Ok(self.command(
            to_value(method, result)?,
            vec![ResourceId::new(BLOCKLIST_RESOURCE)],
        ))
    }

    /// A writer's outcome: the revision the daemon is at, and what it touched.
    fn command(&self, result: Value, affected: Vec<ResourceId>) -> Outcome {
        Outcome::command(result, self.canonical.revision().get(), affected)
    }
}

/// `address`, normalised, or `-32602` with the sentence that refuses it.
fn entry_param(params: &Value) -> Result<String, DomainError> {
    let raw = string_param(params, "address")?;
    mp_core::blocklist::normalize(&raw).map_err(|why| invalid_params(why).into())
}

/// Run a file or store operation off the dispatcher's thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DomainError> + Send + 'static,
) -> Result<T, DomainError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| DomainError::internal(format!("the sender task {e}")))?
}

fn to_value<T: serde::Serialize>(method: &str, answer: T) -> Result<Value, DomainError> {
    serde_json::to_value(answer).map_err(|e| internal(format!("serialising {method}: {e}")).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::dispatch::CancelScope;

    /// The family, in method-name order, durable, the list a query and the
    /// two edits commands.
    #[test]
    fn the_family_declares_three_methods_in_name_order() {
        let names: Vec<&str> = SENDER_METHOD_SPECS.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                mp_protocol::sender::METHOD_SENDER_BLOCK,
                mp_protocol::sender::METHOD_SENDER_LIST,
                mp_protocol::sender::METHOD_SENDER_UNBLOCK,
            ]
        );
        for spec in SENDER_METHOD_SPECS {
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            let kind = if spec.name == "sender.list" {
                MethodKind::Query
            } else {
                MethodKind::Command
            };
            assert_eq!(spec.kind, kind, "{}", spec.name);
        }
    }
}
