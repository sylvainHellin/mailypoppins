//! `state.bootstrap`: the whole canonical state, and the revision it was
//! captured at, in one serialized operation (P3a-U4).
//!
//! A client calls this once per connection and then follows `state.event`
//! notifications; it calls it again only when its instance changed, when a
//! revision gap appeared, or when the daemon asked for a resync. The connection
//! is already a subscriber by the time it gets here - the server subscribes it
//! when it accepts the socket - so this call attaches that subscription to the
//! fan-out and captures the state behind the same gate, which is what makes
//! every change around it reach the client exactly once.
//!
//! A [`MethodKind::Query`]: it reports state and moves none, so its outcome
//! carries neither a revision nor an affected resource. The `revision` in the
//! *result* is the capture point the client watermarks from, which is a
//! different fact from "this call moved the daemon to N".

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use log::info;
use serde_json::{json, Value};

use crate::daemon::state::events::fake_event_burst;
use crate::daemon::state::{CanonicalState, Change, ConnectionId};
use crate::daemon::sync_outcome::fake_sync_outcomes;
use mp_protocol::events::SyncCompleted;

use super::super::dispatch::{
    CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
};

/// `state.bootstrap` as the dispatcher serves it.
pub struct StateBootstrap {
    /// The one canonical state of this daemon process.
    pub state: Arc<CanonicalState>,
    /// The test-only readiness delay, read once at startup from
    /// [`FAKE_READY_ENV`](crate::daemon::state::FAKE_READY_ENV). `None` in
    /// every real daemon.
    pub fake_ready: Option<Duration>,
    /// Whether the readiness countdown has already been armed, so a second
    /// bootstrap does not flip the accounts twice.
    armed: AtomicBool,
    /// The test-only event burst, read once at startup from
    /// [`FAKE_EVENT_BURST_ENV`](crate::daemon::state::events::FAKE_EVENT_BURST_ENV).
    /// `None` in every real daemon.
    fake_burst: Option<u64>,
    /// The test-only sync outcomes, read once at startup from
    /// [`FAKE_SYNC_OUTCOME_ENV`](crate::daemon::sync_outcome::FAKE_SYNC_OUTCOME_ENV).
    /// Empty in every real daemon.
    fake_outcomes: Vec<SyncCompleted>,
}

/// The next mailbox slug the burst hook will use, for the life of the process.
///
/// Process-global and never reset, so two bursts never name one mailbox twice
/// and nothing a burst commits can coalesce with anything an earlier one did.
static NEXT_BURST_SLUG: AtomicU64 = AtomicU64::new(0);

impl StateBootstrap {
    /// Build the method around one state and one readiness delay.
    pub fn new(state: Arc<CanonicalState>, fake_ready: Option<Duration>) -> Self {
        StateBootstrap {
            state,
            fake_ready,
            armed: AtomicBool::new(false),
            fake_burst: fake_event_burst(),
            fake_outcomes: fake_sync_outcomes(),
        }
    }

    /// Start the readiness countdown, once per daemon.
    ///
    /// The countdown starts here rather than at daemon startup so a client that
    /// bootstraps cannot lose the race against it: every account is `opening` in
    /// the snapshot it just took, and readiness arrives afterwards as an
    /// ordinary event.
    fn arm_fake_readiness(&self) {
        let Some(delay) = self.fake_ready else {
            return;
        };
        if self.armed.swap(true, Ordering::SeqCst) {
            return;
        }
        let state = Arc::clone(&self.state);
        info!("[daemon] fake readiness armed, {delay:?} after this bootstrap");
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            for account in state.account_names() {
                state.apply(Change::AccountReady { account });
            }
        });
    }

    /// Commit the test-only event burst, once per bootstrap.
    ///
    /// Off the bootstrap's own path and after its revision was captured, so no
    /// bootstrap's latency includes the burst and every burst revision is above
    /// the one the bootstrap reported. The slugs name mailboxes no account has,
    /// which [`CanonicalState::apply`] reduces to nothing while still fanning
    /// the change out: a burst therefore fills queues without touching the
    /// snapshot a second client takes.
    ///
    /// On the blocking pool rather than as an async task: a burst is thousands
    /// of synchronous commits with no await in them, and a worker thread it
    /// owned for that long would be a worker thread no other connection could
    /// use, which is exactly what the slow-client tests measure.
    fn arm_fake_burst(&self) {
        let Some(count) = self.fake_burst else {
            return;
        };
        let Some(account) = self.state.account_names().into_iter().next() else {
            return;
        };
        let first = NEXT_BURST_SLUG.fetch_add(count, Ordering::SeqCst);
        let state = Arc::clone(&self.state);
        info!("[daemon] fake event burst armed, {count} changes after this bootstrap");
        tokio::task::spawn_blocking(move || {
            for index in first..first + count {
                state.apply(Change::MailboxCounts {
                    account: account.clone(),
                    mailbox: format!("burst-{index}"),
                    total: index,
                    unread: 0,
                    badge: 0,
                });
            }
        });
    }

    /// Commit the test-only sync outcomes, once per bootstrap.
    ///
    /// Off the bootstrap's own path and after its revision was captured, so
    /// every outcome lands above the revision the bootstrap reported and a
    /// client that watermarked at it applies all of them in order. Each one is
    /// addressed to the first configured account, whatever `account` the JSON
    /// carried: the hook exists to exercise the wire, not to invent accounts
    /// the daemon does not have, and a change naming an unknown account would
    /// be dropped by [`CanonicalState::apply`]'s reducer.
    ///
    /// On the blocking pool for the burst's reason: these are synchronous
    /// commits with no await in them.
    fn arm_fake_outcomes(&self) {
        if self.fake_outcomes.is_empty() {
            return;
        }
        let Some(account) = self.state.account_names().into_iter().next() else {
            return;
        };
        let outcomes = self.fake_outcomes.clone();
        let state = Arc::clone(&self.state);
        info!(
            "[daemon] fake sync outcomes armed, {} after this bootstrap",
            outcomes.len()
        );
        tokio::task::spawn_blocking(move || {
            for mut outcome in outcomes {
                outcome.account = account.clone();
                state.apply(Change::SyncCompleted(outcome));
            }
        });
    }
}

impl Method for StateBootstrap {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new("state.bootstrap", MethodKind::Query, 1)
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        _params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let (snapshot, revision, instance) =
                self.state.bootstrap(ConnectionId(ctx.connection_id));
            self.arm_fake_readiness();
            self.arm_fake_burst();
            self.arm_fake_outcomes();
            Ok(Outcome::query(json!({
                "instance_id": instance.as_str(),
                "revision": revision.get(),
                // What this connection agreed on at the handshake, not the
                // daemon's whole list: a client acts on what it may use here.
                "capabilities": ctx.capabilities,
                "snapshot": snapshot.to_json(),
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::dispatch::ClientKind;
    use crate::daemon::state::{AccountSeed, InstanceId};

    fn ctx(capabilities: &[&str]) -> ClientCtx {
        ClientCtx {
            connection_id: 4,
            kind: ClientKind::Gui,
            protocol: 1,
            capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn method() -> StateBootstrap {
        StateBootstrap::new(
            Arc::new(CanonicalState::new(
                InstanceId::new("abcd"),
                vec![AccountSeed {
                    name: "alpha".to_string(),
                    mailboxes: Vec::new(),
                }],
            )),
            None,
        )
    }

    /// The result names the instance, the capture revision, this connection's
    /// capabilities and the snapshot, and nothing else.
    #[tokio::test]
    async fn the_result_carries_the_four_documented_members() {
        let method = method();
        let outcome = method
            .call(&ctx(&["daemon.status"]), json!({}), CancelToken::new())
            .await
            .expect("a bootstrap answers");
        let mut keys: Vec<&str> = outcome
            .result
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["capabilities", "instance_id", "revision", "snapshot"]
        );
        assert_eq!(outcome.result["instance_id"], json!("abcd"));
        assert_eq!(outcome.result["capabilities"], json!(["daemon.status"]));
        assert_eq!(
            outcome.result["snapshot"]["accounts"][0]["state"],
            json!("opening")
        );
    }

    /// A query moves nothing, so its outcome carries no revision and no
    /// affected resource, whatever the `revision` inside its result says.
    #[tokio::test]
    async fn a_bootstrap_is_a_query_outcome() {
        let outcome = method()
            .call(&ctx(&[]), json!({}), CancelToken::new())
            .await
            .expect("a bootstrap answers");
        assert_eq!(outcome.revision, None);
        assert!(outcome.affected.is_empty());
        assert!(outcome.result["revision"].as_u64().expect("a revision") > 0);
    }
}
