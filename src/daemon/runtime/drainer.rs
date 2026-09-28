//! The per-account drainer (#0133): a debounced drain of the outbox and the
//! mutation queue after an interactive mutation.
//!
//! A `settle: false` mutation - every one the TUI makes - commits the row
//! change and the op it owes the server in one transaction and answers without
//! touching the server (#0039, P5-U6). Until this module, only a sync tick
//! drained that queue, and nothing scheduled a tick after a local mutation: the
//! watcher fires only when the *server* moves, and a queued archive has not
//! moved it. An archive therefore stayed local until a manual sync or unrelated
//! new mail.
//!
//! The mutation now calls [`AccountRuntime::request_drain`], which records the
//! request and returns, and the drainer runs [`AccountRuntime::drain`] once the
//! requests have been quiet for [`DRAIN_DEBOUNCE`]. The debounce trails: every
//! request restarts the wait, so a thousand-row selection, which is one call
//! per row, is one drain after the last row rather than a thousand.
//!
//! # What it publishes
//!
//! A drain whose ops all landed changed nothing a client holds: the row change
//! was committed and seen when the mutation answered. A drain that rolled ops
//! back did, and it commits a [`Change::MutationsRolledBack`], which a client
//! reads the way it reads a tick's `failed_mutations`: a warning line and a
//! reload of the account's rows. It never publishes a `sync.completed`, which
//! would move the `last_sync` ledger for a drain that ran no sync.
//!
//! Every drain that ran then commits one [`Change::MailboxCounts`] per mailbox
//! whose counts differ from what the canonical state holds. The mutation itself
//! publishes none, so this is where the sidebar of every client converges on
//! what the queued mutations and any rollback left.
//!
//! # Lifetime
//!
//! The watcher's rules, for the watcher's reasons: spawned for a ready runtime
//! with a server, holding it weakly, stopped by its retirement or its drop, and
//! never looking a runtime up by name. A retired runtime also refuses the drain
//! itself, so a request that raced the retirement drains nothing.

use std::sync::{Arc, Weak};
use std::time::Duration;

use log::{debug, info, warn};
use mp_protocol::events::MutationsRolledBack;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::AccountConfig;
use crate::daemon::state::{CanonicalState, Change};

use super::account::{AccountRuntime, DrainOutcome, Readiness};

/// How long the mutation requests of one account must be quiet before the
/// drainer drains it.
///
/// Long enough that a burst of keystrokes, or a bulk action issuing one call
/// per row, is one drain; short enough that the server has the change before
/// the user looks at another client.
pub const DRAIN_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Start the drainer for `runtime`, if it can drain.
///
/// A local-only account owes no server anything, and a blocked runtime holds
/// no engine lock: neither gets a task. The handle is the test's.
pub fn spawn(
    runtime: &Arc<AccountRuntime>,
    canonical: Arc<CanonicalState>,
    cfg: &AccountConfig,
) -> Option<JoinHandle<()>> {
    if cfg.is_local_only() || runtime.readiness() != Readiness::Ready {
        debug!("[drainer] {} has nothing to drain", cfg.name);
        return None;
    }
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.drain_requests(),
        DRAIN_DEBOUNCE,
        Arc::new(move |runtime: &AccountRuntime, outcome: DrainOutcome| {
            let canonical = Arc::clone(&canonical);
            let account = runtime.account().to_string();
            Box::pin(async move { publish(&canonical, &account, &outcome).await })
                as futures::future::BoxFuture<'static, ()>
        }),
    )))
}

/// What the drainer does with a drain that ran: the production one commits to
/// the canonical state, a test's records.
type Publish = Arc<
    dyn Fn(&AccountRuntime, DrainOutcome) -> futures::future::BoxFuture<'static, ()> + Send + Sync,
>;

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// One account's drainer, until the runtime it was spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    mut requests: watch::Receiver<u64>,
    quiet: Duration,
    publish: Publish,
) {
    let account = runtime
        .upgrade()
        .map(|runtime| runtime.account().to_string())
        .unwrap_or_default();
    debug!("[drainer] draining {account} on request");
    loop {
        // The first request of a burst. A request made while the previous
        // drain ran is already marked, so it is answered here at once.
        tokio::select! {
            () = stopped(&mut retired) => break,
            changed = requests.changed() => if changed.is_err() { break },
        }
        // The trailing debounce: every request restarts the wait.
        loop {
            tokio::select! {
                () = stopped(&mut retired) => return finish(&account),
                changed = requests.changed() => {
                    if changed.is_err() {
                        return finish(&account);
                    }
                }
                () = tokio::time::sleep(quiet) => break,
            }
        }
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let outcome = runtime.drain().await;
        if outcome.ran {
            debug!("[drainer] drained {account}{}", outcome.status);
            publish(&runtime, outcome).await;
        }
    }
    finish(&account);
}

fn finish(account: &str) {
    info!("[drainer] {account}'s runtime retired; stopped draining");
}

/// Commit what a drain that ran changed.
async fn publish(canonical: &Arc<CanonicalState>, account: &str, outcome: &DrainOutcome) {
    commit_rollback(canonical, account, outcome.failed_mutations);
    publish_changed_counts(canonical, account).await;
}

/// Commit a [`Change::MutationsRolledBack`] when the drain rolled anything
/// back, and nothing when it did not.
fn commit_rollback(canonical: &CanonicalState, account: &str, failed: u64) {
    if failed > 0 {
        canonical.apply(Change::MutationsRolledBack(MutationsRolledBack {
            account: account.to_string(),
            failed,
        }));
    }
}

/// Commit one [`Change::MailboxCounts`] per mailbox whose store counts differ
/// from what `canonical` holds for it.
///
/// Compared against the canonical state rather than a memory of this task's
/// own, because the watcher and a server fetch publish counts too, and a
/// private memory would miss a mailbox that moved away and back between two
/// drains.
async fn publish_changed_counts(canonical: &Arc<CanonicalState>, account: &str) {
    let name = account.to_string();
    let read = tokio::task::spawn_blocking(move || {
        let store = crate::store::Store::open(crate::config::store_path(&name))?;
        crate::store::read::mailbox_read_counts(&store, &name)
    })
    .await;
    let fresh = match read {
        Ok(Ok(fresh)) => fresh,
        Ok(Err(e)) => return warn!("[drainer] counting the mailboxes of {account}: {e:#}"),
        Err(e) => return warn!("[drainer] the count task for {account} {e}"),
    };
    commit_changed_counts(
        canonical,
        account,
        fresh
            .into_iter()
            .map(|(mailbox, counts)| (mailbox, (counts.total as u64, counts.unread as u64))),
    );
}

/// The comparison and the commits of [`publish_changed_counts`], apart from
/// the store read so a test can drive it.
fn commit_changed_counts(
    canonical: &CanonicalState,
    account: &str,
    fresh: impl IntoIterator<Item = (String, (u64, u64))>,
) {
    for (mailbox, pair) in fresh {
        if canonical.mailbox_counts(account, &mailbox) == Some(pair) {
            continue;
        }
        canonical.apply(Change::MailboxCounts {
            account: account.to_string(),
            mailbox,
            total: pair.0,
            unread: pair.1,
            // What the sidebar prints beside the label, which is the total
            // (`src/daemon/methods/mailbox.rs`).
            badge: pair.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use futures::future::FutureExt;

    use super::super::account::{DrainHook, Phase, TickHooks};
    use crate::engine_lock::EngineLock;

    /// A short quiet period, so the tests run in real time without a paused
    /// clock: the production value is [`DRAIN_DEBOUNCE`], and only its length
    /// differs.
    const QUIET: Duration = Duration::from_millis(150);

    /// Hooks that count how often each drain slot is entered.
    fn counting_hooks(entered: &Arc<Mutex<Vec<Phase>>>) -> TickHooks {
        let record = |entered: Arc<Mutex<Vec<Phase>>>| -> DrainHook {
            Arc::new(move |ctx| {
                entered.lock().unwrap().push(ctx.phase);
                async { String::new() }.boxed()
            })
        };
        TickHooks {
            outbox: record(Arc::clone(entered)),
            mutations: record(Arc::clone(entered)),
            body: Arc::new(|_ctx| async { Ok(()) }.boxed()),
        }
    }

    fn config() -> AccountConfig {
        AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        }
    }

    fn seeded_state() -> CanonicalState {
        use crate::daemon::state::{AccountSeed, MailboxSeed};
        CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            vec![AccountSeed {
                name: "alpha".to_string(),
                mailboxes: ["inbox", "archive"]
                    .into_iter()
                    .map(|slug| MailboxSeed {
                        role: slug.to_string(),
                        slug: slug.to_string(),
                        label: slug.to_string(),
                    })
                    .collect(),
            }],
        )
    }

    /// Only a mailbox whose counts differ from the state's is committed, so a
    /// drain that moved nothing publishes nothing.
    #[test]
    fn only_moved_counts_are_committed() {
        let state = seeded_state();
        let before = state.revision().get();
        let fresh = || {
            vec![
                ("inbox".to_string(), (4, 1)),
                ("archive".to_string(), (0, 0)),
            ]
        };
        commit_changed_counts(&state, "alpha", fresh());
        assert_eq!(
            state.revision().get(),
            before + 1,
            "the archive was already 0/0"
        );
        assert_eq!(state.mailbox_counts("alpha", "inbox"), Some((4, 1)));

        commit_changed_counts(&state, "alpha", fresh());
        assert_eq!(
            state.revision().get(),
            before + 1,
            "nothing moved the second time"
        );
    }

    /// A drain that rolled mutations back tells every client, as a lifecycle
    /// event of its own and never as a `sync.completed`; one that rolled
    /// nothing back says nothing.
    #[tokio::test]
    async fn a_rollback_is_published_and_a_clean_drain_is_not() {
        use crate::daemon::state::ConnectionId;
        let state = Arc::new(seeded_state());
        let conn = ConnectionId(1);
        let mut queue = state.subscribe(conn);
        state.bootstrap(conn);

        commit_rollback(&state, "alpha", 0);
        assert!(queue.drain().is_empty());

        commit_rollback(&state, "alpha", 3);
        let changes: Vec<Change> = queue
            .drain()
            .into_iter()
            .map(|(_, change)| change)
            .collect();
        assert_eq!(
            changes,
            vec![Change::MutationsRolledBack(MutationsRolledBack {
                account: "alpha".to_string(),
                failed: 3,
            })]
        );
        assert_eq!(changes[0].kind(), "mutations.rolled_back");
        assert!(state.last_sync("alpha").is_none(), "a drain is not a sync");
    }

    /// The drainer over `runtime`, with a publish that counts the drains it is
    /// handed.
    fn drainer(runtime: &Arc<AccountRuntime>, published: &Arc<AtomicUsize>) -> JoinHandle<()> {
        let published = Arc::clone(published);
        tokio::spawn(run(
            Arc::downgrade(runtime),
            runtime.retired(),
            runtime.drain_requests(),
            QUIET,
            Arc::new(move |_runtime: &AccountRuntime, _outcome: DrainOutcome| {
                published.fetch_add(1, Ordering::SeqCst);
                async {}.boxed()
            }),
        ))
    }

    fn drains(entered: &Mutex<Vec<Phase>>) -> Vec<Phase> {
        entered.lock().unwrap().clone()
    }

    /// A burst of requests closer together than the quiet period is one drain,
    /// run once the burst stops, and it enters the outbox before the queue.
    #[tokio::test]
    async fn a_burst_of_requests_is_one_drain_after_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, counting_hooks(&entered))
                .unwrap(),
        );
        let published = Arc::new(AtomicUsize::new(0));
        let task = drainer(&runtime, &published);

        for _ in 0..10 {
            runtime.request_drain();
            tokio::time::sleep(QUIET / 5).await;
        }
        // Still inside the quiet period of the last request: nothing yet.
        assert!(
            drains(&entered).is_empty(),
            "the debounce trails the last request"
        );

        tokio::time::sleep(QUIET * 4).await;
        assert_eq!(
            drains(&entered),
            vec![Phase::DrainOutbox, Phase::DrainMutations]
        );
        assert_eq!(published.load(Ordering::SeqCst), 1);

        // A later request is a second, separate drain.
        runtime.request_drain();
        tokio::time::sleep(QUIET * 4).await;
        assert_eq!(drains(&entered).len(), 4);
        assert_eq!(published.load(Ordering::SeqCst), 2);

        drop(runtime);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a dropped runtime stops its drainer")
            .unwrap();
    }

    /// One request drains after the quiet period, and not before it.
    #[tokio::test]
    async fn a_single_request_drains_after_the_quiet_period() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, counting_hooks(&entered))
                .unwrap(),
        );
        let published = Arc::new(AtomicUsize::new(0));
        let _task = drainer(&runtime, &published);

        let asked = tokio::time::Instant::now();
        runtime.request_drain();
        tokio::time::sleep(QUIET / 2).await;
        assert!(drains(&entered).is_empty());
        tokio::time::timeout(Duration::from_secs(5), async {
            while drains(&entered).len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the drain ran");
        assert!(
            asked.elapsed() >= QUIET,
            "the drain waited out the quiet period"
        );
    }

    /// A request made before the drainer subscribed is not lost.
    #[tokio::test]
    async fn a_request_before_the_drainer_started_still_drains() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, counting_hooks(&entered))
                .unwrap(),
        );
        runtime.request_drain();
        let published = Arc::new(AtomicUsize::new(0));
        let _task = drainer(&runtime, &published);
        tokio::time::sleep(QUIET * 4).await;
        assert_eq!(drains(&entered).len(), 2);
    }

    /// A runtime retired inside the quiet period drains nothing, and its
    /// drainer stops; a request after the retirement is not even recorded.
    #[tokio::test]
    async fn a_retired_runtime_drains_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, counting_hooks(&entered))
                .unwrap(),
        );
        let published = Arc::new(AtomicUsize::new(0));
        let task = drainer(&runtime, &published);

        runtime.request_drain();
        assert!(runtime.retire(Duration::from_secs(1)).await);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a retired runtime stops its drainer")
            .unwrap();
        runtime.request_drain();
        tokio::time::sleep(QUIET * 3).await;
        assert!(drains(&entered).is_empty());
        assert_eq!(published.load(Ordering::SeqCst), 0);

        // And the drain itself refuses, for a request that raced the flag.
        assert!(!runtime.drain().await.ran);
        assert!(drains(&entered).is_empty());
    }

    /// A blocked runtime gets no drainer and refuses a drain it is asked for
    /// directly: it holds no engine lock.
    #[tokio::test]
    async fn a_blocked_runtime_drains_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let holder = EngineLock::try_acquire_at(&dir.path().join("store.lock"), "alpha")
            .unwrap()
            .expect("a free lock");
        let entered = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(
            AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, counting_hooks(&entered))
                .unwrap(),
        );
        assert!(matches!(runtime.readiness(), Readiness::Blocked { .. }));

        let canonical = Arc::new(CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            Vec::new(),
        ));
        let mut cfg = config();
        cfg.imap.host = "imap.example.com".to_string();
        assert!(!cfg.is_local_only());
        assert!(spawn(&runtime, canonical, &cfg).is_none());

        // Even a drainer forced onto it drains nothing.
        let published = Arc::new(AtomicUsize::new(0));
        let _task = drainer(&runtime, &published);
        runtime.request_drain();
        tokio::time::sleep(QUIET * 3).await;
        assert!(!runtime.drain().await.ran);
        assert!(drains(&entered).is_empty());
        assert_eq!(published.load(Ordering::SeqCst), 0);
        drop(holder);
    }

    /// A drain requested while a tick runs waits for the tick and then drains,
    /// rather than running beside it or being skipped.
    #[tokio::test]
    async fn a_drain_waits_for_a_running_tick_and_then_runs() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = counting_hooks(&entered);
        let (release, gate) = watch::channel(false);
        let body_entered = Arc::clone(&entered);
        hooks.body = Arc::new(move |ctx| {
            body_entered.lock().unwrap().push(ctx.phase);
            let mut gate = gate.clone();
            async move {
                let _ = gate.wait_for(|open| *open).await;
                Ok(())
            }
            .boxed()
        });
        let runtime =
            Arc::new(AccountRuntime::start_at_with_hooks(dir.path(), config(), 1, hooks).unwrap());

        let ticking = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move { runtime.tick(super::super::account::TickKind::Quick).await })
        };
        // Wait until the tick is inside its body.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !drains(&entered).contains(&Phase::Body) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        let draining = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move { runtime.drain().await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !drains(&entered).contains(&Phase::DrainOutbox),
            "the drain does not run beside the tick"
        );

        release.send_replace(true);
        let outcome = tokio::time::timeout(Duration::from_secs(5), draining)
            .await
            .unwrap()
            .unwrap();
        assert!(outcome.ran);
        ticking.await.unwrap();
        assert_eq!(
            drains(&entered),
            vec![
                Phase::HeadOutbox,
                Phase::HeadMutations,
                Phase::Body,
                Phase::TailOutbox,
                Phase::TailMutations,
                Phase::DrainOutbox,
                Phase::DrainMutations,
            ]
        );
    }
}
