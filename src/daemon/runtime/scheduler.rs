//! The per-account scheduler (#0134): a quick tick once an account has gone
//! `imap.sync_interval_secs` without one.
//!
//! The watcher ([`super::watcher`]) hears one mailbox: an IMAP IDLE round on
//! INBOX, or the Graph inbox's id set. Everything else stayed stale until a
//! manual sync or new INBOX mail: another mailbox changed by another client, a
//! queued op or an outbox Sent copy waiting out its backoff, a body fetch the
//! deadline cut, an IDLE connection the server dropped without a word, and a
//! daemon with no client at all. The scheduler is what reaches those: it runs
//! the same [`TickKind::Quick`] tick the watcher runs, through the same
//! [`tick_and_publish`], so its `sync.completed` and its counts read exactly
//! like a watcher's.
//!
//! # The interval is measured from the last tick, whoever asked for it
//!
//! The runtime records when each tick that ran a body finished
//! ([`AccountRuntime::completed_ticks`]), and the scheduler waits until that
//! moment plus the interval. A manual `mp sync`, a watcher tick, a client's
//! startup fetch and the scheduler's own tick all move it, so a busy account is
//! never ticked twice in a row for the schedule's sake. A drain is not a tick
//! and does not move it. The first scheduled tick is an interval after the
//! runtime started.
//!
//! A scheduled tick that finds a tick running joins it, and one that finds a
//! drain running waits for its turn: both are [`AccountRuntime::tick`]'s own
//! rules. A tick that did not run (refused) does not move the record, so the
//! scheduler also remembers when it last fired and never fires sooner than an
//! interval after that.
//!
//! # What gets a scheduler
//!
//! A ready runtime with an IMAP server and a non-zero interval. A local-only
//! account has no server; a blocked runtime holds no engine lock, and the
//! engine that does is scheduling its own; a Graph account's daemon tick has no
//! Graph backend yet (`sync_once` refuses it), so a scheduled one would report
//! a failed sync every interval.
//!
//! # Lifetime
//!
//! The watcher's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop, and never looking a runtime up by name. A reload
//! that changes `sync_interval_secs` changes the effective account, which
//! restarts the runtime and with it the scheduler, so the new interval needs
//! no plumbing of its own.

use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use log::{debug, info};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::{AccountConfig, AuthMethod};
use crate::daemon::state::CanonicalState;

use super::account::{AccountRuntime, Readiness, TickKind};
use super::publish::tick_and_publish;

/// Start the scheduler for `runtime`, if it has anything to schedule.
///
/// The handle is the test's; the daemon lets the task run until the runtime
/// it was spawned for retires.
pub fn spawn(
    runtime: &Arc<AccountRuntime>,
    canonical: Arc<CanonicalState>,
    cfg: &AccountConfig,
) -> Option<JoinHandle<()>> {
    let interval = schedulable(runtime, cfg)?;
    info!(
        "[scheduler] ticking {} after {}s without a sync",
        cfg.name,
        interval.as_secs()
    );
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.completed_ticks(),
        interval,
        Arc::new(move |runtime: Arc<AccountRuntime>| {
            let canonical = Arc::clone(&canonical);
            Box::pin(async move {
                info!(
                    "[scheduler] {} has gone its interval without a sync; ticking",
                    runtime.account()
                );
                tick_and_publish(&runtime, &canonical, TickKind::Quick).await;
            }) as BoxFuture<'static, ()>
        }),
    )))
}

/// The interval `runtime` is scheduled at, or `None` when it gets no
/// scheduler (see the module docs).
fn schedulable(runtime: &AccountRuntime, cfg: &AccountConfig) -> Option<Duration> {
    let why_not = if cfg.is_local_only() {
        "has no server"
    } else if runtime.readiness() != Readiness::Ready {
        "is blocked"
    } else if cfg.auth_method == AuthMethod::Graph {
        "is a Graph account, which the daemon tick cannot sync yet"
    } else if cfg.imap.sync_interval().is_none() {
        "has sync_interval_secs = 0"
    } else {
        return cfg.imap.sync_interval();
    };
    debug!("[scheduler] not scheduling {}: it {why_not}", cfg.name);
    None
}

/// What the scheduler does when the interval is up: the production one ticks
/// and publishes, a test's counts.
type Fire = Arc<dyn Fn(Arc<AccountRuntime>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// One account's scheduler, until the runtime it was spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    mut ticks: watch::Receiver<Instant>,
    interval: Duration,
    fire: Fire,
) {
    let account = runtime
        .upgrade()
        .map(|runtime| runtime.account().to_string())
        .unwrap_or_default();
    // A floor under the next deadline that the scheduler's own firing sets, so
    // a tick that was refused and moved nothing is not fired again at once.
    let mut fired: Option<Instant> = None;
    loop {
        let last = *ticks.borrow_and_update();
        let due = fired.map_or(last, |fired| fired.max(last)) + interval;
        tokio::select! {
            // Checked first, so a retired runtime never fires a deadline that
            // expired at the same moment.
            biased;
            () = stopped(&mut retired) => break,
            // Another tick finished: the deadline moves with it.
            changed = ticks.changed() => {
                if changed.is_err() {
                    break;
                }
                continue;
            }
            () = tokio::time::sleep_until(due) => {}
        }
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        fired = Some(Instant::now());
        fire(runtime).await;
    }
    info!("[scheduler] {account}'s runtime retired; stopped scheduling");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::FutureExt;

    use super::super::account::{DrainHook, TickHooks};
    use crate::engine_lock::EngineLock;

    /// A short interval, so the tests run in real time without a paused clock:
    /// the production value is the configured one, and only its length
    /// differs.
    const INTERVAL: Duration = Duration::from_millis(300);

    /// Hooks whose body counts the ticks that ran one.
    fn counting_hooks(bodies: &Arc<AtomicUsize>) -> TickHooks {
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        let bodies = Arc::clone(bodies);
        TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(move |_ctx| {
                bodies.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }.boxed()
            }),
        }
    }

    fn config() -> AccountConfig {
        let mut cfg = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        cfg.imap.host = "imap.example.com".to_string();
        cfg
    }

    fn started(dir: &std::path::Path, bodies: &Arc<AtomicUsize>) -> Arc<AccountRuntime> {
        Arc::new(
            AccountRuntime::start_at_with_hooks(dir, config(), 1, counting_hooks(bodies)).unwrap(),
        )
    }

    /// The scheduler over `runtime`, firing a real quick tick and counting the
    /// times it fired.
    fn scheduler(runtime: &Arc<AccountRuntime>, fired: &Arc<AtomicUsize>) -> JoinHandle<()> {
        let fired = Arc::clone(fired);
        tokio::spawn(run(
            Arc::downgrade(runtime),
            runtime.retired(),
            runtime.completed_ticks(),
            INTERVAL,
            Arc::new(move |runtime: Arc<AccountRuntime>| {
                fired.fetch_add(1, Ordering::SeqCst);
                async move {
                    runtime.tick(TickKind::Quick).await;
                }
                .boxed()
            }),
        ))
    }

    fn canonical() -> Arc<CanonicalState> {
        Arc::new(CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            Vec::new(),
        ))
    }

    /// Nothing before the interval, one tick after it, and the next one an
    /// interval after that one finished.
    #[tokio::test]
    async fn a_tick_fires_after_the_interval_and_again_an_interval_later() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let task = scheduler(&runtime, &fired);

        tokio::time::sleep(INTERVAL / 2).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "not before the interval");
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(
            bodies.load(Ordering::SeqCst),
            1,
            "the scheduled tick ran a body"
        );
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(fired.load(Ordering::SeqCst), 2);

        drop(runtime);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a dropped runtime stops its scheduler")
            .unwrap();
    }

    /// A tick of another origin (a manual sync, the watcher) pushes the
    /// scheduled one back a whole interval from when it finished.
    #[tokio::test]
    async fn a_tick_of_another_origin_pushes_the_scheduled_one_back() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let _task = scheduler(&runtime, &fired);

        tokio::time::sleep(INTERVAL * 2 / 3).await;
        let manual = Instant::now();
        runtime.tick(TickKind::Full).await;
        // Past the deadline the runtime's start set, short of the one the
        // manual tick set.
        tokio::time::sleep_until(manual + INTERVAL * 2 / 3).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "the manual tick reset it");
        tokio::time::sleep_until(manual + INTERVAL * 3 / 2).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    /// A drain is not a tick: it does not move the deadline.
    #[tokio::test]
    async fn a_drain_does_not_push_the_scheduled_tick_back() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let before = *runtime.completed_ticks().borrow();
        assert!(runtime.drain().await.ran);
        assert_eq!(*runtime.completed_ticks().borrow(), before);
        runtime.tick(TickKind::Quick).await;
        assert!(*runtime.completed_ticks().borrow() > before, "a tick does");
    }

    /// `sync_interval_secs = 0` schedules nothing, and so does a local-only or
    /// a Graph account.
    #[tokio::test]
    async fn a_zero_interval_schedules_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        assert!(
            schedulable(&runtime, &config()).is_some(),
            "the default schedules"
        );

        let mut off = config();
        off.imap.sync_interval_secs = 0;
        assert!(spawn(&runtime, canonical(), &off).is_none());

        let local = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        assert!(local.is_local_only());
        assert!(spawn(&runtime, canonical(), &local).is_none());

        let mut graph = config();
        graph.auth_method = AuthMethod::Graph;
        assert!(spawn(&runtime, canonical(), &graph).is_none());
    }

    /// A retired runtime fires nothing, even with its deadline long past, and
    /// its scheduler stops.
    #[tokio::test]
    async fn a_retired_runtime_schedules_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        let fired = Arc::new(AtomicUsize::new(0));
        let task = scheduler(&runtime, &fired);

        assert!(runtime.retire(Duration::from_secs(1)).await);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a retired runtime stops its scheduler")
            .unwrap();
        tokio::time::sleep(INTERVAL * 2).await;
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(bodies.load(Ordering::SeqCst), 0);
    }

    /// A blocked runtime gets no scheduler; one forced onto it fires at most
    /// once an interval, since its refused tick moves no deadline, and runs no
    /// body.
    #[tokio::test]
    async fn a_blocked_runtime_schedules_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let holder = EngineLock::try_acquire_at(&dir.path().join("store.lock"), "alpha")
            .unwrap()
            .expect("a free lock");
        let bodies = Arc::new(AtomicUsize::new(0));
        let runtime = started(dir.path(), &bodies);
        assert!(matches!(runtime.readiness(), Readiness::Blocked { .. }));
        assert!(spawn(&runtime, canonical(), &config()).is_none());

        let fired = Arc::new(AtomicUsize::new(0));
        let _task = scheduler(&runtime, &fired);
        tokio::time::sleep(INTERVAL * 5 / 2).await;
        assert!(
            fired.load(Ordering::SeqCst) <= 2,
            "no hot loop on a tick that moves no deadline"
        );
        assert_eq!(bodies.load(Ordering::SeqCst), 0);
        drop(holder);
    }
}
